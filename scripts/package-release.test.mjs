#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";
import {
    existsSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    readdirSync,
    rmSync,
    statSync,
    writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
    REQUIRED_STDLIB_MODULES,
    buildBundle,
    detectBundleTarget,
    readVersion,
} from "./bundle.mjs";
import {
    assertSafeArtifactPath,
    checksumText,
    collectBundleEntries,
    createArchiveBuffer,
    decodeArchiveBuffer,
    decodeTarGzEntries,
    decodeZipEntries,
    encodeTarGzEntries,
    encodeZipEntries,
    extractArchiveEntries,
    packageBundle,
    releaseArtifactManifest,
    releaseArtifactNames,
    sha256,
    validateArchiveEntries,
    verifyChecksum,
} from "./package-release.mjs";

const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const canonicalVersion = readVersion(join(repositoryRoot, "Cargo.toml"));
const currentTarget = detectBundleTarget();
const releaseBinary = join(
    repositoryRoot,
    "target",
    "release",
    currentTarget.binaryName,
);

function commandReportsVersion(command, expected) {
    if (!existsSync(command)) return false;
    const result = spawnSync(command, ["--version"], {
        encoding: "utf8",
        windowsHide: true,
    });
    return result.status === 0 && result.stdout.trim() === expected;
}

function hasCurrentReleaseBinary() {
    return commandReportsVersion(releaseBinary, `aster ${canonicalVersion}`);
}

function temporaryDirectory(label) {
    return mkdtempSync(join(tmpdir(), `aster-package-${label}-`));
}

test("release binary gate requires the canonical reported version", () => {
    assert.equal(commandReportsVersion(process.execPath, process.version), true);
    assert.equal(commandReportsVersion(process.execPath, "v0.0.0-stale"), false);
    assert.equal(commandReportsVersion(join(tmpdir(), "missing-aster"), canonicalVersion), false);
});

function createFakeWorkspace(root, binaryName) {
    mkdirSync(join(root, "target", "release"), { recursive: true });
    writeFileSync(join(root, "target", "release", binaryName), "fake-binary");
    writeFileSync(join(root, "LICENSE"), "Apache-2.0 test license\n");
    for (const module of REQUIRED_STDLIB_MODULES) {
        const path = join(root, "stdlib", ...module.split("/"));
        mkdirSync(dirname(path), { recursive: true });
        writeFileSync(path, `// ${module}\n`);
    }
}

function createFakeBundle({
    target = "windows-x64",
    binaryName = target.startsWith("windows-") ? "aster.exe" : "aster",
    version = "1.2.3",
} = {}) {
    const workspaceRoot = temporaryDirectory("workspace");
    createFakeWorkspace(workspaceRoot, binaryName);
    const distRoot = join(workspaceRoot, "dist");
    const bundle = buildBundle({
        workspaceRoot,
        distRoot,
        version,
        bundleTarget: target,
        binaryName,
    });
    return { workspaceRoot, distRoot, version, target, binaryName, ...bundle };
}

function minimalEntries(root = "aster-1.2.3-windows-x64", binary = "aster.exe") {
    return [
        { name: `${root}/`, type: "directory", mode: 0o755 },
        { name: `${root}/LICENSE`, type: "file", mode: 0o644, data: Buffer.from("license") },
        { name: `${root}/bin/`, type: "directory", mode: 0o755 },
        { name: `${root}/bin/${binary}`, type: "file", mode: 0o755, data: Buffer.from("binary") },
        {
            name: `${root}/install-manifest.json`,
            type: "file",
            mode: 0o644,
            data: Buffer.from("{}\n"),
        },
        { name: `${root}/stdlib/`, type: "directory", mode: 0o755 },
        { name: `${root}/stdlib/aster/`, type: "directory", mode: 0o755 },
        {
            name: `${root}/stdlib/aster/core.aster`,
            type: "file",
            mode: 0o644,
            data: Buffer.from("// core\n"),
        },
    ];
}

test("release names cover Windows ZIP and Linux double-extension TAR.GZ", () => {
    assert.deepEqual(releaseArtifactNames("1.2.3", "windows-x64"), {
        rootName: "aster-1.2.3-windows-x64",
        archiveName: "aster-1.2.3-windows-x64.zip",
        checksumName: "aster-1.2.3-windows-x64.zip.sha256",
        manifestName: "release-artifact.json",
    });
    assert.deepEqual(releaseArtifactNames("1.2.3", "linux-x64"), {
        rootName: "aster-1.2.3-linux-x64",
        archiveName: "aster-1.2.3-linux-x64.tar.gz",
        checksumName: "aster-1.2.3-linux-x64.tar.gz.sha256",
        manifestName: "release-artifact.json",
    });
    assert.match(canonicalVersion, /^\d+\.\d+\.\d+$/);
});

test("checksum sidecar is exact, lowercase, basename-only, and detects tampering", () => {
    const archive = Buffer.from("deterministic archive");
    const hash = sha256(archive);
    const text = checksumText(hash, "aster-1.2.3-windows-x64.zip");
    assert.match(
        text,
        /^[0-9a-f]{64}  aster-1\.2\.3-windows-x64\.zip\n$/,
    );
    assert.equal(
        verifyChecksum(archive, text, "aster-1.2.3-windows-x64.zip"),
        hash,
    );
    assert.throws(
        () =>
            verifyChecksum(
                Buffer.from("tampered"),
                text,
                "aster-1.2.3-windows-x64.zip",
            ),
        /does not match/,
    );
    assert.throws(() => checksumText(hash, "../archive.zip"), /safe basename/);
});

test("release artifact manifest is stable, complete, relative, and timestamp-free", () => {
    const hash = "a".repeat(64);
    const text = releaseArtifactManifest({
        version: "1.2.3",
        target: "linux-x64",
        archiveName: "aster-1.2.3-linux-x64.tar.gz",
        hash,
        size: 123456,
    });
    assert.ok(text.endsWith("\n"));
    const manifest = JSON.parse(text);
    assert.deepEqual(manifest, {
        schema: 1,
        product: "aster",
        version: "1.2.3",
        target: "linux-x64",
        archive: "aster-1.2.3-linux-x64.tar.gz",
        sha256: hash,
        size: 123456,
    });
    assert.ok(!text.includes("timestamp"));
    assert.ok(!text.includes(resolve(repositoryRoot)));
});

test("artifact path guard accepts only exact files below repository dist/artifacts", () => {
    const root = temporaryDirectory("paths with spaces ç");
    const artifacts = join(root, "dist", "artifacts");
    const archive = join(artifacts, "aster-1.2.3-windows-x64.zip");
    try {
        assert.doesNotThrow(() =>
            assertSafeArtifactPath({
                workspaceRoot: root,
                artifactsDir: artifacts,
                candidate: archive,
                expectedBasename: "aster-1.2.3-windows-x64.zip",
            }),
        );
        assert.throws(
            () =>
                assertSafeArtifactPath({
                    workspaceRoot: root,
                    artifactsDir: artifacts,
                    candidate: join(artifacts, "..", "outside.zip"),
                    expectedBasename: "outside.zip",
                }),
            /expected file/,
        );
        assert.throws(
            () =>
                assertSafeArtifactPath({
                    workspaceRoot: root,
                    artifactsDir: root,
                    candidate: join(root, "archive.zip"),
                    expectedBasename: "archive.zip",
                }),
            /must be/,
        );
        assert.throws(
            () =>
                assertSafeArtifactPath({
                    workspaceRoot: root,
                    artifactsDir: artifacts,
                    candidate: archive,
                    expectedBasename: "../archive.zip",
                }),
            /unsafe/,
        );
    } finally {
        rmSync(root, { recursive: true, force: true });
    }
});

test("ZIP encoding is byte deterministic and preserves normalized names and modes", () => {
    const entries = minimalEntries();
    const first = encodeZipEntries(entries);
    const second = encodeZipEntries([...entries].reverse());
    assert.deepEqual(first, second);
    const decoded = decodeZipEntries(first);
    validateArchiveEntries(decoded, {
        rootName: "aster-1.2.3-windows-x64",
        binaryName: "aster.exe",
        expectedNames: entries.map((entry) => entry.name),
    });
    assert.equal(
        decoded.find((entry) => entry.name.endsWith("/bin/aster.exe")).mode,
        0o755,
    );
});

test("TAR.GZ encoding is byte deterministic and preserves Linux executable mode", () => {
    const root = "aster-1.2.3-linux-x64";
    const entries = minimalEntries(root, "aster");
    const first = encodeTarGzEntries(entries);
    const second = encodeTarGzEntries([...entries].reverse());
    assert.deepEqual(first, second);
    assert.equal(
        first.readUInt32LE(4),
        Date.parse("2000-01-01T00:00:00Z") / 1000,
        "gzip mtime must use the fixed archive date",
    );
    const decoded = decodeTarGzEntries(first);
    validateArchiveEntries(decoded, {
        rootName: root,
        binaryName: "aster",
        expectedNames: entries.map((entry) => entry.name),
    });
    assert.equal(
        decoded.find((entry) => entry.name.endsWith("/bin/aster")).mode,
        0o755,
    );
    assert.deepEqual(
        decoded.filter((entry) => entry.type === "directory").map((entry) => entry.name),
        [
            `${root}/`,
            `${root}/bin/`,
            `${root}/stdlib/`,
            `${root}/stdlib/aster/`,
        ],
        "the official TAR contract includes typed directory entries",
    );
});

test("archive decoder dispatches only the format matching the target", () => {
    const windows = minimalEntries();
    const linux = minimalEntries("aster-1.2.3-linux-x64", "aster");
    assert.deepEqual(
        decodeArchiveBuffer(createArchiveBuffer(windows, "windows-x64"), "windows-x64").map(
            (entry) => entry.name,
        ),
        [...windows].sort((left, right) =>
            Buffer.from(left.name).compare(Buffer.from(right.name)),
        ).map((entry) => entry.name),
    );
    assert.deepEqual(
        decodeArchiveBuffer(createArchiveBuffer(linux, "linux-x64"), "linux-x64").map(
            (entry) => entry.name,
        ),
        [...linux].sort((left, right) =>
            Buffer.from(left.name).compare(Buffer.from(right.name)),
        ).map((entry) => entry.name),
    );
});

test("archive validation rejects absolute, traversal, backslash, duplicate, and symlink entries", async (context) => {
    const valid = minimalEntries();
    const cases = [
        ["absolute", { name: "/outside", type: "file", data: Buffer.alloc(0) }, /absolute/],
        ["drive", { name: "C:/outside", type: "file", data: Buffer.alloc(0) }, /absolute/],
        ["traversal", { name: "root/../outside", type: "file", data: Buffer.alloc(0) }, /unsafe/],
        ["backslash", { name: "root\\..\\outside", type: "file", data: Buffer.alloc(0) }, /backslash/],
        ["symlink", { name: "root/link", type: "symlink", linkName: "../outside" }, /symbolic link/],
    ];
    for (const [name, entry, pattern] of cases) {
        await context.test(name, () => {
            assert.throws(() => validateArchiveEntries([entry]), pattern);
        });
    }
    await context.test("duplicate", () => {
        assert.throws(
            () => validateArchiveEntries([...valid, valid[0]]),
            /duplicate/,
        );
    });
});

test("archive validation requires one root, required files, and exact bundle inventory", () => {
    const root = "aster-1.2.3-windows-x64";
    const entries = minimalEntries(root);
    assert.throws(
        () =>
            validateArchiveEntries(entries.slice(1), {
                rootName: root,
                binaryName: "aster.exe",
            }),
        /required entry missing/,
    );
    assert.throws(
        () =>
            validateArchiveEntries(
                [
                    ...entries,
                    {
                        name: "other-root/file",
                        type: "file",
                        data: Buffer.alloc(0),
                    },
                ],
                { rootName: root, binaryName: "aster.exe" },
            ),
        /exactly one/,
    );
    assert.throws(
        () =>
            validateArchiveEntries(
                [
                    ...entries,
                    {
                        name: `${root}/unexpected.log`,
                        type: "file",
                        data: Buffer.alloc(0),
                    },
                ],
                {
                    rootName: root,
                    binaryName: "aster.exe",
                    expectedNames: entries.map((entry) => entry.name),
                },
            ),
        /inventory/,
    );
});

test("archive decoders reject absent or malformed archives", () => {
    assert.throws(() => decodeZipEntries(Buffer.alloc(0)), /end record/);
    assert.throws(() => decodeTarGzEntries(Buffer.from("not gzip")), /gzip/);
});

test("safe extraction supports spaces and Unicode and refuses a non-empty destination", () => {
    const destination = join(temporaryDirectory("extract parent"), "saída com espaços ç");
    const entries = minimalEntries();
    try {
        extractArchiveEntries(entries, destination);
        assert.equal(
            readFileSync(
                join(destination, "aster-1.2.3-windows-x64", "LICENSE"),
                "utf8",
            ),
            "license",
        );
        assert.throws(
            () => extractArchiveEntries(entries, destination),
            /must be empty/,
        );
    } finally {
        rmSync(dirname(destination), { recursive: true, force: true });
    }
});

test("collectBundleEntries rejects missing bundles and returns a single exact root", () => {
    const fake = createFakeBundle();
    try {
        const entries = collectBundleEntries(
            fake.bundleDir,
            fake.dirName,
            fake.binaryName,
        );
        assert.ok(entries.every((entry) => entry.name.startsWith(`${fake.dirName}/`) || entry.name === `${fake.dirName}/`));
        assert.throws(
            () =>
                collectBundleEntries(
                    join(fake.workspaceRoot, "missing"),
                    fake.dirName,
                    fake.binaryName,
                ),
            /not found/,
        );
    } finally {
        rmSync(fake.workspaceRoot, { recursive: true, force: true });
    }
});

test("packageBundle creates only the three expected artifacts and rebuilds them deterministically", () => {
    const fake = createFakeBundle();
    try {
        const first = packageBundle({
            workspaceRoot: fake.workspaceRoot,
            distRoot: fake.distRoot,
            bundleDir: fake.bundleDir,
            version: fake.version,
            target: fake.target,
            binaryName: fake.binaryName,
        });
        const firstArchive = readFileSync(first.archivePath);
        const firstChecksum = readFileSync(first.checksumPath, "utf8");
        const firstManifest = readFileSync(first.manifestPath, "utf8");

        writeFileSync(first.archivePath, "stale");
        writeFileSync(first.checksumPath, "stale");
        writeFileSync(first.manifestPath, "stale");

        const second = packageBundle({
            workspaceRoot: fake.workspaceRoot,
            distRoot: fake.distRoot,
            bundleDir: fake.bundleDir,
            version: fake.version,
            target: fake.target,
            binaryName: fake.binaryName,
        });
        assert.deepEqual(readFileSync(second.archivePath), firstArchive);
        assert.equal(readFileSync(second.checksumPath, "utf8"), firstChecksum);
        assert.equal(readFileSync(second.manifestPath, "utf8"), firstManifest);
        assert.deepEqual(readdirSync(first.artifactsDir).sort(), [
            first.archiveName,
            `${first.archiveName}.sha256`,
            "release-artifact.json",
        ].sort());
        assert.equal(first.hash, second.hash);
        assert.equal(first.size, second.size);
    } finally {
        rmSync(fake.workspaceRoot, { recursive: true, force: true });
    }
});

test("packageBundle Linux archive keeps the canonical typed directory inventory", () => {
    const fake = createFakeBundle({ target: "linux-x64", binaryName: "aster" });
    try {
        const result = packageBundle({
            workspaceRoot: fake.workspaceRoot,
            distRoot: fake.distRoot,
            bundleDir: fake.bundleDir,
            version: fake.version,
            target: fake.target,
            binaryName: fake.binaryName,
        });
        const entries = decodeTarGzEntries(readFileSync(result.archivePath));
        const directories = entries
            .filter((entry) => entry.type === "directory")
            .map((entry) => entry.name);
        assert.ok(directories.includes(`${fake.dirName}/`));
        assert.ok(directories.includes(`${fake.dirName}/bin/`));
        assert.ok(directories.includes(`${fake.dirName}/stdlib/`));
        assert.ok(directories.includes(`${fake.dirName}/stdlib/aster/`));
        assert.ok(
            entries.some(
                (entry) =>
                    entry.name === `${fake.dirName}/bin/aster` &&
                    entry.type === "file" &&
                    entry.mode === 0o755,
            ),
        );
    } finally {
        rmSync(fake.workspaceRoot, { recursive: true, force: true });
    }
});

test("packageBundle removes partial artifacts after invalid bundle manifest", () => {
    const fake = createFakeBundle();
    const manifestPath = join(fake.bundleDir, "install-manifest.json");
    try {
        writeFileSync(manifestPath, "{ invalid");
        assert.throws(
            () =>
                packageBundle({
                    workspaceRoot: fake.workspaceRoot,
                    distRoot: fake.distRoot,
                    bundleDir: fake.bundleDir,
                    version: fake.version,
                    target: fake.target,
                    binaryName: fake.binaryName,
                }),
            /not valid JSON/,
        );
        const artifacts = join(fake.distRoot, "artifacts");
        assert.deepEqual(existsSync(artifacts) ? readdirSync(artifacts) : [], []);
    } finally {
        rmSync(fake.workspaceRoot, { recursive: true, force: true });
    }
});

test(
    "real extracted archive is relocatable and proves external stdlib use",
    {
        skip: hasCurrentReleaseBinary()
            ? false
            : "current release binary is absent; npm run bundle builds it before package:release",
    },
    () => {
        const workspaceRoot = temporaryDirectory("real relocation");
        const distRoot = join(workspaceRoot, "dist");
        try {
            const bundle = buildBundle({
                workspaceRoot: repositoryRoot,
                distRoot,
                version: canonicalVersion,
                bundleTarget: currentTarget.bundleTarget,
                binaryName: currentTarget.binaryName,
            });
            const result = packageBundle({
                workspaceRoot,
                distRoot,
                bundleDir: bundle.bundleDir,
                version: canonicalVersion,
                target: currentTarget.bundleTarget,
                binaryName: currentTarget.binaryName,
                verifyCli: true,
            });
            assert.equal(result.relocation.version, `aster ${canonicalVersion}`);
            assert.equal(result.relocation.runOutput, "40");
            assert.ok(statSync(result.archivePath).size > 0);
        } finally {
            rmSync(workspaceRoot, { recursive: true, force: true });
        }
    },
);
