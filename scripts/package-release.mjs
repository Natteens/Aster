#!/usr/bin/env node
//! Deterministic release archive builder for ASTER install bundles.

import {
    chmodSync,
    cpSync,
    existsSync,
    lstatSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    readdirSync,
    renameSync,
    rmSync,
    statSync,
    writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import {
    basename,
    dirname,
    isAbsolute,
    join,
    relative,
    resolve,
    sep,
} from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync, gzipSync } from "node:zlib";

import {
    PRODUCT,
    REQUIRED_STDLIB_MODULES,
    bundleDirectoryName,
    detectBundleTarget,
    readVersion,
    validateBundle,
} from "./bundle.mjs";

export const RELEASE_ARTIFACT_SCHEMA = 1;
export const FIXED_ARCHIVE_DATE = "2000-01-01T00:00:00Z";

const FIXED_UNIX_TIME = Date.parse(FIXED_ARCHIVE_DATE) / 1000;
const FIXED_DOS_TIME = 0;
const FIXED_DOS_DATE = (20 << 9) | (1 << 5) | 1;
const ZIP_UTF8_FLAG = 0x0800;
const ZIP_STORE_METHOD = 0;
const ZIP_VERSION = 20;
const ZIP_VERSION_MADE_BY_UNIX = (3 << 8) | ZIP_VERSION;
const TAR_BLOCK_SIZE = 512;
const FILE_MODE = 0o644;
const EXECUTABLE_MODE = 0o755;
const DIRECTORY_MODE = 0o755;

const CRC32_TABLE = (() => {
    const table = new Uint32Array(256);
    for (let index = 0; index < table.length; index += 1) {
        let value = index;
        for (let bit = 0; bit < 8; bit += 1) {
            value = (value & 1) !== 0 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
        }
        table[index] = value >>> 0;
    }
    return table;
})();

function crc32(buffer) {
    let crc = 0xffffffff;
    for (const byte of buffer) {
        crc = CRC32_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
    }
    return (crc ^ 0xffffffff) >>> 0;
}

function checkUInt32(value, label) {
    if (!Number.isSafeInteger(value) || value < 0 || value > 0xffffffff) {
        throw new Error(`${label} exceeds the supported archive size`);
    }
    return value;
}

function writeAscii(buffer, offset, length, value, label) {
    const encoded = Buffer.from(value, "ascii");
    if (encoded.length > length) {
        throw new Error(`${label} is too long for the archive format`);
    }
    encoded.copy(buffer, offset);
}

function writeTarOctal(buffer, offset, length, value, label) {
    const octal = value.toString(8);
    if (octal.length > length - 1) {
        throw new Error(`${label} exceeds the TAR field width`);
    }
    writeAscii(buffer, offset, length, `${octal.padStart(length - 1, "0")}\0`, label);
}

function readTarOctal(buffer, offset, length, label) {
    const raw = buffer
        .subarray(offset, offset + length)
        .toString("ascii")
        .replace(/\0.*$/s, "")
        .trim();
    if (raw === "") return 0;
    if (!/^[0-7]+$/.test(raw)) {
        throw new Error(`Archive validation failed: invalid TAR ${label}`);
    }
    const value = Number.parseInt(raw, 8);
    if (!Number.isSafeInteger(value)) {
        throw new Error(`Archive validation failed: TAR ${label} is too large`);
    }
    return value;
}

function nullTerminated(buffer, offset, length) {
    const field = buffer.subarray(offset, offset + length);
    const end = field.indexOf(0);
    return field.subarray(0, end === -1 ? field.length : end).toString("utf8");
}

function splitTarPath(path) {
    const encoded = Buffer.byteLength(path);
    if (encoded <= 100) return { name: path, prefix: "" };
    for (let slash = path.lastIndexOf("/"); slash > 0; slash = path.lastIndexOf("/", slash - 1)) {
        const prefix = path.slice(0, slash);
        const name = path.slice(slash + 1);
        if (Buffer.byteLength(prefix) <= 155 && Buffer.byteLength(name) <= 100) {
            return { name, prefix };
        }
    }
    throw new Error(`Archive path is too long for deterministic USTAR: ${path}`);
}

function normalizeArchiveEntry(entry) {
    const type = entry.type;
    if (!["file", "directory", "symlink"].includes(type)) {
        throw new Error(`Archive validation failed: unsupported entry type "${type}"`);
    }
    const data = type === "file" ? Buffer.from(entry.data ?? Buffer.alloc(0)) : Buffer.alloc(0);
    return {
        name: entry.name,
        type,
        mode:
            entry.mode ??
            (type === "directory" ? DIRECTORY_MODE : type === "symlink" ? 0o777 : FILE_MODE),
        data,
        linkName: entry.linkName ?? "",
    };
}

export function releaseArtifactNames(version, target) {
    if (!version || !target || version.includes("/") || target.includes("/")) {
        throw new Error("Version and target must be non-empty archive name components");
    }
    const rootName = `${PRODUCT}-${version}-${target}`;
    let extension;
    if (target === "windows-x64") {
        extension = ".zip";
    } else if (target === "linux-x64") {
        extension = ".tar.gz";
    } else {
        throw new Error(`Unsupported release artifact target: ${target}`);
    }
    const archiveName = `${rootName}${extension}`;
    return {
        rootName,
        archiveName,
        checksumName: `${archiveName}.sha256`,
        manifestName: "release-artifact.json",
    };
}

export function sha256(buffer) {
    return createHash("sha256").update(buffer).digest("hex");
}

export function checksumText(hash, archiveName) {
    if (!/^[0-9a-f]{64}$/.test(hash)) {
        throw new Error("SHA-256 must contain 64 lowercase hexadecimal characters");
    }
    if (!archiveName || basename(archiveName) !== archiveName || archiveName.includes("..")) {
        throw new Error("Checksum archive name must be a safe basename");
    }
    return `${hash}  ${archiveName}\n`;
}

export function verifyChecksum(archiveBuffer, sidecarText, expectedArchiveName) {
    const match = sidecarText.match(/^([0-9a-f]{64})  ([^\r\n]+)\n$/);
    if (!match) throw new Error("Checksum file has an invalid format");
    if (match[2] !== expectedArchiveName || basename(match[2]) !== match[2]) {
        throw new Error("Checksum filename does not match the archive");
    }
    const actual = sha256(archiveBuffer);
    if (match[1] !== actual) throw new Error("Checksum does not match the archive");
    return actual;
}

export function releaseArtifactManifest({ version, target, archiveName, hash, size }) {
    if (basename(archiveName) !== archiveName || isAbsolute(archiveName)) {
        throw new Error("Release artifact archive must be a relative basename");
    }
    if (!Number.isSafeInteger(size) || size < 0) {
        throw new Error("Release artifact size must be a non-negative integer");
    }
    return (
        JSON.stringify(
            {
                schema: RELEASE_ARTIFACT_SCHEMA,
                product: PRODUCT,
                version,
                target,
                archive: archiveName,
                sha256: hash,
                size,
            },
            null,
            2,
        ) + "\n"
    );
}

function assertNotSymlink(path, label) {
    if (existsSync(path) && lstatSync(path).isSymbolicLink()) {
        throw new Error(`${label} must not be a symbolic link`);
    }
}

export function assertSafeArtifactPath({
    workspaceRoot,
    artifactsDir,
    candidate,
    expectedBasename,
}) {
    if (!workspaceRoot || !artifactsDir || !candidate || !expectedBasename) {
        throw new Error("Artifact path validation requires non-empty paths");
    }
    if (
        expectedBasename.includes("..") ||
        expectedBasename.includes("/") ||
        expectedBasename.includes("\\") ||
        basename(expectedBasename) !== expectedBasename
    ) {
        throw new Error("Artifact basename is unsafe");
    }

    const repository = resolve(workspaceRoot);
    const expectedArtifacts = resolve(repository, "dist", "artifacts");
    const actualArtifacts = resolve(artifactsDir);
    const finalPath = resolve(candidate);
    if (actualArtifacts !== expectedArtifacts) {
        throw new Error("Artifacts directory must be <repository>/dist/artifacts");
    }
    if (actualArtifacts === repository || dirname(actualArtifacts) === actualArtifacts) {
        throw new Error("Artifacts directory cannot be the repository or filesystem root");
    }
    const rel = relative(actualArtifacts, finalPath);
    if (
        rel !== expectedBasename ||
        rel.startsWith("..") ||
        isAbsolute(rel) ||
        basename(finalPath) !== expectedBasename
    ) {
        throw new Error("Artifact path is not the expected file inside dist/artifacts");
    }

    assertNotSymlink(join(repository, "dist"), "dist/");
    assertNotSymlink(actualArtifacts, "dist/artifacts/");
    assertNotSymlink(finalPath, expectedBasename);
}

function removeExpectedArtifact(options) {
    assertSafeArtifactPath(options);
    if (existsSync(options.candidate)) rmSync(options.candidate, { force: true });
}

function validateEntryName(name) {
    if (typeof name !== "string" || name.length === 0 || name.includes("\0")) {
        throw new Error("Archive validation failed: entry name is empty or invalid");
    }
    if (name.includes("\\")) {
        throw new Error(`Archive validation failed: backslash is not allowed: ${name}`);
    }
    if (name.startsWith("/") || /^[A-Za-z]:\//.test(name)) {
        throw new Error(`Archive validation failed: absolute entry path: ${name}`);
    }
    const path = name.endsWith("/") ? name.slice(0, -1) : name;
    const components = path.split("/");
    if (
        components.some(
            (component) => component === "" || component === "." || component === "..",
        )
    ) {
        throw new Error(`Archive validation failed: unsafe entry path: ${name}`);
    }
}

export function validateArchiveEntries(
    rawEntries,
    { rootName, binaryName, expectedNames } = {},
) {
    if (!Array.isArray(rawEntries) || rawEntries.length === 0) {
        throw new Error("Archive validation failed: archive is empty");
    }
    const entries = rawEntries.map(normalizeArchiveEntry);
    const seen = new Set();
    for (const entry of entries) {
        validateEntryName(entry.name);
        if (seen.has(entry.name)) {
            throw new Error(`Archive validation failed: duplicate entry: ${entry.name}`);
        }
        seen.add(entry.name);
        if (entry.type === "symlink") {
            throw new Error(`Archive validation failed: symbolic link is not allowed: ${entry.name}`);
        }
    }

    const roots = new Set(entries.map((entry) => entry.name.replace(/\/$/, "").split("/")[0]));
    if (roots.size !== 1 || (rootName && !roots.has(rootName))) {
        throw new Error("Archive validation failed: archive must have exactly one expected root");
    }

    if (rootName && binaryName) {
        const required = [
            `${rootName}/`,
            `${rootName}/bin/`,
            `${rootName}/bin/${binaryName}`,
            `${rootName}/stdlib/`,
            `${rootName}/stdlib/aster/`,
            `${rootName}/LICENSE`,
            `${rootName}/install-manifest.json`,
        ];
        for (const name of required) {
            if (!seen.has(name)) {
                throw new Error(`Archive validation failed: required entry missing: ${name}`);
            }
        }
    }

    if (expectedNames) {
        const expected = new Set(expectedNames);
        if (expected.size !== seen.size) {
            throw new Error("Archive validation failed: archive inventory differs from bundle");
        }
        for (const name of expected) {
            if (!seen.has(name)) {
                throw new Error(`Archive validation failed: bundle entry missing: ${name}`);
            }
        }
    }
    return entries;
}

function walkBundleDirectory(directory, archivePrefix, binaryName) {
    const entries = [];
    const visit = (diskPath, archivePath) => {
        const metadata = lstatSync(diskPath);
        if (metadata.isSymbolicLink()) {
            throw new Error(`Bundle contains an unsupported symbolic link: ${archivePath}`);
        }
        if (metadata.isDirectory()) {
            entries.push({
                name: `${archivePath}/`,
                type: "directory",
                mode: DIRECTORY_MODE,
                data: Buffer.alloc(0),
            });
            const children = readdirSync(diskPath).sort((left, right) =>
                Buffer.from(left).compare(Buffer.from(right)),
            );
            for (const child of children) {
                visit(join(diskPath, child), `${archivePath}/${child}`);
            }
            return;
        }
        if (!metadata.isFile()) {
            throw new Error(`Bundle contains an unsupported entry: ${archivePath}`);
        }
        entries.push({
            name: archivePath,
            type: "file",
            mode: archivePath.endsWith(`/bin/${binaryName}`)
                ? EXECUTABLE_MODE
                : FILE_MODE,
            data: readFileSync(diskPath),
        });
    };
    visit(directory, archivePrefix);
    return entries;
}

export function collectBundleEntries(bundleDir, rootName, binaryName) {
    if (!existsSync(bundleDir) || !lstatSync(bundleDir).isDirectory()) {
        throw new Error(`Bundle directory not found: ${bundleDir}`);
    }
    if (lstatSync(bundleDir).isSymbolicLink()) {
        throw new Error("Bundle directory must not be a symbolic link");
    }
    return walkBundleDirectory(bundleDir, rootName, binaryName);
}

export function encodeZipEntries(rawEntries) {
    const entries = rawEntries.map(normalizeArchiveEntry).sort((a, b) =>
        Buffer.from(a.name).compare(Buffer.from(b.name)),
    );
    if (entries.length > 0xffff) {
        throw new Error("ZIP archive contains too many entries");
    }
    const localParts = [];
    const centralParts = [];
    let offset = 0;

    for (const entry of entries) {
        const name = Buffer.from(entry.name, "utf8");
        const data = entry.data;
        const checksum = crc32(data);
        checkUInt32(data.length, "ZIP entry");
        checkUInt32(offset, "ZIP offset");

        const local = Buffer.alloc(30);
        local.writeUInt32LE(0x04034b50, 0);
        local.writeUInt16LE(ZIP_VERSION, 4);
        local.writeUInt16LE(ZIP_UTF8_FLAG, 6);
        local.writeUInt16LE(ZIP_STORE_METHOD, 8);
        local.writeUInt16LE(FIXED_DOS_TIME, 10);
        local.writeUInt16LE(FIXED_DOS_DATE, 12);
        local.writeUInt32LE(checksum, 14);
        local.writeUInt32LE(data.length, 18);
        local.writeUInt32LE(data.length, 22);
        local.writeUInt16LE(name.length, 26);
        local.writeUInt16LE(0, 28);
        localParts.push(local, name, data);

        const central = Buffer.alloc(46);
        central.writeUInt32LE(0x02014b50, 0);
        central.writeUInt16LE(ZIP_VERSION_MADE_BY_UNIX, 4);
        central.writeUInt16LE(ZIP_VERSION, 6);
        central.writeUInt16LE(ZIP_UTF8_FLAG, 8);
        central.writeUInt16LE(ZIP_STORE_METHOD, 10);
        central.writeUInt16LE(FIXED_DOS_TIME, 12);
        central.writeUInt16LE(FIXED_DOS_DATE, 14);
        central.writeUInt32LE(checksum, 16);
        central.writeUInt32LE(data.length, 20);
        central.writeUInt32LE(data.length, 24);
        central.writeUInt16LE(name.length, 28);
        central.writeUInt16LE(0, 30);
        central.writeUInt16LE(0, 32);
        central.writeUInt16LE(0, 34);
        central.writeUInt16LE(0, 36);
        const fileType =
            entry.type === "directory"
                ? 0o040000
                : entry.type === "symlink"
                  ? 0o120000
                  : 0o100000;
        const dosAttributes = entry.type === "directory" ? 0x10 : 0;
        central.writeUInt32LE(
            (((fileType | entry.mode) << 16) | dosAttributes) >>> 0,
            38,
        );
        central.writeUInt32LE(offset, 42);
        centralParts.push(central, name);

        offset += local.length + name.length + data.length;
    }

    const centralOffset = offset;
    const centralBuffer = Buffer.concat(centralParts);
    const localBuffer = Buffer.concat(localParts);
    const end = Buffer.alloc(22);
    end.writeUInt32LE(0x06054b50, 0);
    end.writeUInt16LE(0, 4);
    end.writeUInt16LE(0, 6);
    end.writeUInt16LE(entries.length, 8);
    end.writeUInt16LE(entries.length, 10);
    end.writeUInt32LE(checkUInt32(centralBuffer.length, "ZIP central directory"), 12);
    end.writeUInt32LE(checkUInt32(centralOffset, "ZIP central offset"), 16);
    end.writeUInt16LE(0, 20);
    return Buffer.concat([localBuffer, centralBuffer, end]);
}

export function decodeZipEntries(buffer) {
    if (buffer.length < 22 || buffer.readUInt32LE(buffer.length - 22) !== 0x06054b50) {
        throw new Error("Archive validation failed: ZIP end record not found");
    }
    const end = buffer.length - 22;
    const count = buffer.readUInt16LE(end + 10);
    const centralSize = buffer.readUInt32LE(end + 12);
    const centralOffset = buffer.readUInt32LE(end + 16);
    if (centralOffset + centralSize !== end) {
        throw new Error("Archive validation failed: invalid ZIP central directory bounds");
    }

    const entries = [];
    let cursor = centralOffset;
    for (let index = 0; index < count; index += 1) {
        if (cursor + 46 > end || buffer.readUInt32LE(cursor) !== 0x02014b50) {
            throw new Error("Archive validation failed: invalid ZIP central entry");
        }
        const flags = buffer.readUInt16LE(cursor + 8);
        const method = buffer.readUInt16LE(cursor + 10);
        const expectedCrc = buffer.readUInt32LE(cursor + 16);
        const compressedSize = buffer.readUInt32LE(cursor + 20);
        const size = buffer.readUInt32LE(cursor + 24);
        const nameLength = buffer.readUInt16LE(cursor + 28);
        const extraLength = buffer.readUInt16LE(cursor + 30);
        const commentLength = buffer.readUInt16LE(cursor + 32);
        const externalAttributes = buffer.readUInt32LE(cursor + 38);
        const localOffset = buffer.readUInt32LE(cursor + 42);
        const recordEnd = cursor + 46 + nameLength + extraLength + commentLength;
        if (recordEnd > end) {
            throw new Error("Archive validation failed: truncated ZIP central entry");
        }
        const nameBytes = buffer.subarray(cursor + 46, cursor + 46 + nameLength);
        const name = nameBytes.toString("utf8");
        if (!Buffer.from(name, "utf8").equals(nameBytes)) {
            throw new Error("Archive validation failed: ZIP entry name is not valid UTF-8");
        }
        if (method !== ZIP_STORE_METHOD || (flags & ZIP_UTF8_FLAG) === 0) {
            throw new Error("Archive validation failed: unsupported ZIP compression or encoding");
        }
        if (compressedSize !== size) {
            throw new Error("Archive validation failed: ZIP stored size mismatch");
        }
        if (localOffset + 30 > centralOffset || buffer.readUInt32LE(localOffset) !== 0x04034b50) {
            throw new Error("Archive validation failed: invalid ZIP local entry");
        }
        if (
            buffer.readUInt16LE(localOffset + 6) !== flags ||
            buffer.readUInt16LE(localOffset + 8) !== method ||
            buffer.readUInt32LE(localOffset + 14) !== expectedCrc ||
            buffer.readUInt32LE(localOffset + 18) !== compressedSize ||
            buffer.readUInt32LE(localOffset + 22) !== size
        ) {
            throw new Error("Archive validation failed: ZIP local metadata differs");
        }
        const localNameLength = buffer.readUInt16LE(localOffset + 26);
        const localExtraLength = buffer.readUInt16LE(localOffset + 28);
        const localName = buffer
            .subarray(localOffset + 30, localOffset + 30 + localNameLength)
            .toString("utf8");
        if (localName !== name) {
            throw new Error("Archive validation failed: ZIP local and central names differ");
        }
        const dataStart = localOffset + 30 + localNameLength + localExtraLength;
        const dataEnd = dataStart + size;
        if (dataEnd > centralOffset) {
            throw new Error("Archive validation failed: ZIP entry data is out of bounds");
        }
        const data = Buffer.from(buffer.subarray(dataStart, dataEnd));
        if (crc32(data) !== expectedCrc) {
            throw new Error("Archive validation failed: ZIP entry checksum mismatch");
        }
        const unixMode = externalAttributes >>> 16;
        const fileType = unixMode & 0o170000;
        const type =
            fileType === 0o120000
                ? "symlink"
                : fileType === 0o040000 || name.endsWith("/")
                  ? "directory"
                  : "file";
        entries.push({
            name,
            type,
            mode: unixMode & 0o777,
            data,
            linkName: type === "symlink" ? data.toString("utf8") : "",
        });
        cursor = recordEnd;
    }
    if (cursor !== end) {
        throw new Error("Archive validation failed: unexpected ZIP central data");
    }
    return entries;
}

export function encodeTarGzEntries(rawEntries) {
    const entries = rawEntries.map(normalizeArchiveEntry).sort((a, b) =>
        Buffer.from(a.name).compare(Buffer.from(b.name)),
    );
    const parts = [];
    for (const entry of entries) {
        const header = Buffer.alloc(TAR_BLOCK_SIZE);
        const archiveName = entry.name.endsWith("/") ? entry.name.slice(0, -1) : entry.name;
        const { name, prefix } = splitTarPath(archiveName);
        writeAscii(header, 0, 100, name, "TAR name");
        writeTarOctal(header, 100, 8, entry.mode, "mode");
        writeTarOctal(header, 108, 8, 0, "uid");
        writeTarOctal(header, 116, 8, 0, "gid");
        writeTarOctal(header, 124, 12, entry.data.length, "size");
        writeTarOctal(header, 136, 12, FIXED_UNIX_TIME, "mtime");
        header.fill(0x20, 148, 156);
        header[156] =
            entry.type === "directory" ? 0x35 : entry.type === "symlink" ? 0x32 : 0x30;
        if (entry.type === "symlink") {
            writeAscii(header, 157, 100, entry.linkName, "TAR link name");
        }
        writeAscii(header, 257, 6, "ustar\0", "TAR magic");
        writeAscii(header, 263, 2, "00", "TAR version");
        writeAscii(header, 345, 155, prefix, "TAR prefix");
        const checksum = header.reduce((sum, byte) => sum + byte, 0);
        writeAscii(
            header,
            148,
            8,
            `${checksum.toString(8).padStart(6, "0")}\0 `,
            "TAR checksum",
        );
        parts.push(header, entry.data);
        const padding =
            entry.data.length % TAR_BLOCK_SIZE === 0
                ? 0
                : TAR_BLOCK_SIZE - (entry.data.length % TAR_BLOCK_SIZE);
        if (padding > 0) parts.push(Buffer.alloc(padding));
    }
    parts.push(Buffer.alloc(TAR_BLOCK_SIZE * 2));
    const compressed = gzipSync(Buffer.concat(parts), { level: 9, mtime: 0 });
    compressed.writeUInt32LE(FIXED_UNIX_TIME, 4);
    compressed[9] = 255;
    return compressed;
}

export function decodeTarGzEntries(buffer) {
    let tar;
    try {
        tar = gunzipSync(buffer);
    } catch {
        throw new Error("Archive validation failed: invalid gzip stream");
    }
    const entries = [];
    let cursor = 0;
    while (cursor + TAR_BLOCK_SIZE <= tar.length) {
        const header = tar.subarray(cursor, cursor + TAR_BLOCK_SIZE);
        if (header.every((byte) => byte === 0)) break;
        const storedChecksum = readTarOctal(header, 148, 8, "checksum");
        const checksumHeader = Buffer.from(header);
        checksumHeader.fill(0x20, 148, 156);
        const actualChecksum = checksumHeader.reduce((sum, byte) => sum + byte, 0);
        if (storedChecksum !== actualChecksum) {
            throw new Error("Archive validation failed: TAR header checksum mismatch");
        }
        if (nullTerminated(header, 257, 6) !== "ustar") {
            throw new Error("Archive validation failed: unsupported TAR format");
        }
        const name = nullTerminated(header, 0, 100);
        const prefix = nullTerminated(header, 345, 155);
        const fullName = prefix ? `${prefix}/${name}` : name;
        const size = readTarOctal(header, 124, 12, "size");
        const mode = readTarOctal(header, 100, 8, "mode");
        const typeFlag = String.fromCharCode(header[156] || 0x30);
        if (!["0", "2", "5"].includes(typeFlag)) {
            throw new Error(`Archive validation failed: unsupported TAR entry type ${typeFlag}`);
        }
        const type =
            typeFlag === "5" ? "directory" : typeFlag === "2" ? "symlink" : "file";
        const dataStart = cursor + TAR_BLOCK_SIZE;
        const dataEnd = dataStart + size;
        if (dataEnd > tar.length) {
            throw new Error("Archive validation failed: truncated TAR entry");
        }
        entries.push({
            name: type === "directory" ? `${fullName}/` : fullName,
            type,
            mode,
            data: Buffer.from(tar.subarray(dataStart, dataEnd)),
            linkName: nullTerminated(header, 157, 100),
        });
        const paddedSize = Math.ceil(size / TAR_BLOCK_SIZE) * TAR_BLOCK_SIZE;
        cursor = dataStart + paddedSize;
    }
    return entries;
}

export function createArchiveBuffer(entries, target) {
    return target.startsWith("windows-")
        ? encodeZipEntries(entries)
        : encodeTarGzEntries(entries);
}

export function decodeArchiveBuffer(buffer, target) {
    return target.startsWith("windows-")
        ? decodeZipEntries(buffer)
        : decodeTarGzEntries(buffer);
}

function safeExtractionPath(destination, entryName) {
    validateEntryName(entryName);
    const relativeName = entryName.endsWith("/") ? entryName.slice(0, -1) : entryName;
    const path = resolve(destination, ...relativeName.split("/"));
    const rel = relative(resolve(destination), path);
    if (!rel || rel.startsWith("..") || isAbsolute(rel)) {
        throw new Error(`Archive extraction path is unsafe: ${entryName}`);
    }
    return path;
}

export function extractArchiveEntries(rawEntries, destination) {
    if (existsSync(destination) && readdirSync(destination).length !== 0) {
        throw new Error("Archive extraction directory must be empty");
    }
    mkdirSync(destination, { recursive: true });
    const entries = validateArchiveEntries(rawEntries);
    for (const entry of entries) {
        const output = safeExtractionPath(destination, entry.name);
        if (entry.type === "directory") {
            mkdirSync(output, { recursive: true });
            continue;
        }
        mkdirSync(dirname(output), { recursive: true });
        writeFileSync(output, entry.data, { mode: entry.mode });
        if (process.platform !== "win32") chmodSync(output, entry.mode);
    }
}

function compareArchiveToBundle(entries, bundleEntries) {
    const bundleByName = new Map(bundleEntries.map((entry) => [entry.name, entry]));
    for (const entry of entries) {
        const expected = bundleByName.get(entry.name);
        if (!expected || expected.type !== entry.type) {
            throw new Error(`Archive validation failed: entry differs from bundle: ${entry.name}`);
        }
        if (entry.type === "file" && !entry.data.equals(expected.data)) {
            throw new Error(`Archive validation failed: file content differs: ${entry.name}`);
        }
    }
}

function environmentWithoutAsterStdlib() {
    return Object.fromEntries(
        Object.entries(process.env).filter(([key]) => key.toUpperCase() !== "ASTER_STDLIB"),
    );
}

export function verifyRelocatableInstall({
    extractedBundle,
    version,
    binaryName,
}) {
    const relocationParent = mkdtempSync(join(tmpdir(), "aster-release-relocation-"));
    const relocatedBundle = join(relocationParent, "instalação ASTER com espaços");
    const projectDirectory = mkdtempSync(join(tmpdir(), "aster-release-project-"));
    const sourcePath = join(projectDirectory, "main.aster");
    writeFileSync(
        sourcePath,
        "using aster.math; public class Program { public static int Main() { return Math.Max(40, 2); } }\n",
        "utf8",
    );
    cpSync(extractedBundle, relocatedBundle, { recursive: true });
    const binary = join(relocatedBundle, "bin", binaryName);
    const environment = environmentWithoutAsterStdlib();

    const run = (args) =>
        spawnSync(binary, args, {
            cwd: projectDirectory,
            env: environment,
            encoding: "utf8",
            windowsHide: true,
        });

    try {
        const versionResult = run(["--version"]);
        if (versionResult.status !== 0 || !versionResult.stdout.includes(version)) {
            throw new Error(`Relocated aster --version failed: ${versionResult.stderr.trim()}`);
        }
        for (const command of ["check", "dump-hir", "dump-mir"]) {
            const result = run([command, sourcePath]);
            if (result.status !== 0) {
                throw new Error(`Relocated aster ${command} failed: ${result.stderr.trim()}`);
            }
        }
        const runResult = run(["run", sourcePath]);
        if (runResult.status !== 0 || runResult.stdout.trim() !== "40") {
            throw new Error(
                `Relocated aster run failed: ${runResult.stderr.trim() || runResult.stdout.trim()}`,
            );
        }

        const requiredModule = REQUIRED_STDLIB_MODULES[0];
        const modulePath = join(
            relocatedBundle,
            "stdlib",
            ...requiredModule.split("/"),
        );
        const backup = `${modulePath}.package-release-backup`;
        renameSync(modulePath, backup);
        try {
            const broken = run(["check", sourcePath]);
            if (broken.status === 0) {
                throw new Error("Relocated CLI silently fell back after external stdlib removal");
            }
            const error = broken.stderr.toLowerCase();
            if (!error.includes("stdlib") && !error.includes("incomplete")) {
                throw new Error("Relocated CLI did not report an explicit external stdlib error");
            }
        } finally {
            renameSync(backup, modulePath);
        }

        return {
            relocationDirectory: relocatedBundle,
            projectDirectory,
            version: versionResult.stdout.trim(),
            runOutput: runResult.stdout.trim(),
        };
    } finally {
        rmSync(relocationParent, { recursive: true, force: true });
        rmSync(projectDirectory, { recursive: true, force: true });
    }
}

export function packageBundle({
    workspaceRoot,
    distRoot,
    bundleDir,
    version,
    target,
    binaryName,
    verifyCli = false,
}) {
    const { rootName, archiveName, checksumName, manifestName } =
        releaseArtifactNames(version, target);
    const artifactsDir = join(distRoot, "artifacts");
    const paths = {
        archive: join(artifactsDir, archiveName),
        checksum: join(artifactsDir, checksumName),
        manifest: join(artifactsDir, manifestName),
    };
    for (const [kind, path] of Object.entries(paths)) {
        assertSafeArtifactPath({
            workspaceRoot,
            artifactsDir,
            candidate: path,
            expectedBasename:
                kind === "archive"
                    ? archiveName
                    : kind === "checksum"
                      ? checksumName
                      : manifestName,
        });
    }
    mkdirSync(artifactsDir, { recursive: true });
    for (const [kind, path] of Object.entries(paths)) {
        removeExpectedArtifact({
            workspaceRoot,
            artifactsDir,
            candidate: path,
            expectedBasename:
                kind === "archive"
                    ? archiveName
                    : kind === "checksum"
                      ? checksumName
                      : manifestName,
        });
    }

    const extractionDirectory = mkdtempSync(join(tmpdir(), "aster-release-extract-"));
    try {
        validateBundle(bundleDir, { version, bundleTarget: target, binaryName });
        const bundleEntries = collectBundleEntries(bundleDir, rootName, binaryName);
        const archiveBuffer = createArchiveBuffer(bundleEntries, target);
        writeFileSync(paths.archive, archiveBuffer);

        const hash = sha256(readFileSync(paths.archive));
        const checksum = checksumText(hash, archiveName);
        writeFileSync(paths.checksum, checksum, "utf8");
        verifyChecksum(readFileSync(paths.archive), readFileSync(paths.checksum, "utf8"), archiveName);

        const size = statSync(paths.archive).size;
        const manifest = releaseArtifactManifest({
            version,
            target,
            archiveName,
            hash,
            size,
        });
        writeFileSync(paths.manifest, manifest, "utf8");

        const decoded = decodeArchiveBuffer(readFileSync(paths.archive), target);
        const validated = validateArchiveEntries(decoded, {
            rootName,
            binaryName,
            expectedNames: bundleEntries.map((entry) => entry.name),
        });
        compareArchiveToBundle(validated, bundleEntries);
        if (
            target.startsWith("linux-") &&
            validated.find((entry) => entry.name === `${rootName}/bin/${binaryName}`)
                ?.mode !== EXECUTABLE_MODE
        ) {
            throw new Error("Archive validation failed: Linux CLI is not executable");
        }

        extractArchiveEntries(validated, extractionDirectory);
        const extractedBundle = join(extractionDirectory, rootName);
        validateBundle(extractedBundle, {
            version,
            bundleTarget: target,
            binaryName,
        });
        const relocation = verifyCli
            ? verifyRelocatableInstall({ extractedBundle, version, binaryName })
            : null;

        return {
            artifactsDir,
            archivePath: paths.archive,
            checksumPath: paths.checksum,
            manifestPath: paths.manifest,
            archiveName,
            checksum,
            manifest,
            hash,
            size,
            entries: validated,
            relocation,
        };
    } catch (error) {
        for (const [kind, path] of Object.entries(paths)) {
            removeExpectedArtifact({
                workspaceRoot,
                artifactsDir,
                candidate: path,
                expectedBasename:
                    kind === "archive"
                        ? archiveName
                        : kind === "checksum"
                          ? checksumName
                          : manifestName,
            });
        }
        throw error;
    } finally {
        rmSync(extractionDirectory, { recursive: true, force: true });
    }
}

export function createReleaseArtifact(repositoryRoot) {
    const version = readVersion(join(repositoryRoot, "Cargo.toml"));
    const { bundleTarget, binaryName } = detectBundleTarget();
    const distRoot = join(repositoryRoot, "dist");
    const bundleDir = join(
        distRoot,
        bundleDirectoryName(version, bundleTarget),
    );
    if (!existsSync(bundleDir)) {
        throw new Error(
            `Install bundle not found: ${bundleDir}\nRun: npm.cmd run bundle`,
        );
    }
    return packageBundle({
        workspaceRoot: repositoryRoot,
        distRoot,
        bundleDir,
        version,
        target: bundleTarget,
        binaryName,
        verifyCli: true,
    });
}

const isMain = process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
    if (process.argv.length !== 2) {
        console.error("error: package:release does not accept arguments");
        process.exit(1);
    }
    try {
        const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
        const result = createReleaseArtifact(repositoryRoot);
        console.log("\nASTER release artifact created\n");
        console.log(`Version: ${readVersion(join(repositoryRoot, "Cargo.toml"))}`);
        console.log(`Target: ${detectBundleTarget().bundleTarget}`);
        console.log(`Archive: ${result.archivePath}`);
        console.log(`SHA-256: ${result.hash}`);
        console.log(`Size: ${result.size} bytes`);
    } catch (error) {
        console.error(`error: ${error.message}`);
        process.exit(1);
    }
}
