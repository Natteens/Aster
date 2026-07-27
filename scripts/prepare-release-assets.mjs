#!/usr/bin/env node

import {
    chmodSync,
    copyFileSync,
    existsSync,
    lstatSync,
    mkdirSync,
    readFileSync,
    readdirSync,
    renameSync,
    rmSync,
    rmdirSync,
    writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import {
    checksumText,
    releaseArtifactNames,
    sha256,
    verifyChecksum,
} from "./package-release.mjs";

export const RELEASE_MANIFEST_SCHEMA = 1;
export const RELEASE_TARGETS = ["linux-x64", "windows-x64"];
export const INSTALLER_NAMES = [
    "install.ps1",
    "install.sh",
    "uninstall.ps1",
    "uninstall.sh",
];

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const ARTIFACT_KEYS = ["schema", "product", "version", "target", "archive", "sha256", "size"];

function fail(message) {
    throw new Error(message);
}

function isHexSha256(value) {
    return typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
}

function assertPlainBasename(value, label) {
    if (
        typeof value !== "string" ||
        value.length === 0 ||
        value === "." ||
        value === ".." ||
        value.includes("/") ||
        value.includes("\\") ||
        value.includes("\0")
    ) {
        fail(`${label} must be a relative basename`);
    }
}

function assertExactKeys(value, expected, label) {
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
        fail(`${label} must be a JSON object`);
    }
    const actual = Object.keys(value).sort();
    const wanted = [...expected].sort();
    if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
        fail(`${label} contains unexpected or missing fields`);
    }
}

function assertRegularFile(path, label) {
    if (!existsSync(path)) fail(`${label} is missing`);
    const metadata = lstatSync(path);
    if (metadata.isSymbolicLink() || !metadata.isFile()) {
        fail(`${label} must be a regular file`);
    }
}

function assertDirectoryNotSymlink(path, label) {
    if (!existsSync(path)) fail(`${label} is missing`);
    const metadata = lstatSync(path);
    if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
        fail(`${label} must be a real directory`);
    }
}

function assertFixedDirectory(repositoryRoot, candidate, expectedRelative, label) {
    const repository = resolve(repositoryRoot);
    const actual = resolve(candidate);
    const expected = resolve(repository, ...expectedRelative);
    if (actual !== expected) fail(`${label} must be ${expectedRelative.join("/")}`);
    const fromRepository = relative(repository, actual);
    if (
        !fromRepository ||
        fromRepository === ".." ||
        fromRepository.startsWith(`..${sep}`) ||
        resolve(actual) === resolve(repository)
    ) {
        fail(`${label} is outside the repository`);
    }
    let component = repository;
    for (const part of expectedRelative) {
        component = join(component, part);
        if (existsSync(component) && lstatSync(component).isSymbolicLink()) {
            fail(`${label} has a symlinked path component`);
        }
    }
}

function readJson(path, label) {
    assertRegularFile(path, label);
    try {
        return JSON.parse(readFileSync(path, "utf8"));
    } catch {
        fail(`${label} is not valid JSON`);
    }
}

function expectedInputFiles(manifest) {
    return [manifest.archive, `${manifest.archive}.sha256`, "release-artifact.json"].sort();
}

function assertExactDirectoryFiles(directory, expected, label) {
    assertDirectoryNotSymlink(directory, label);
    const actual = readdirSync(directory).sort();
    const wanted = [...expected].sort();
    if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
        const unexpected = actual.find((name) => !wanted.includes(name));
        fail(
            unexpected
                ? `${label} contains unexpected file: ${unexpected}`
                : `${label} is missing a required file`,
        );
    }
    for (const name of actual) assertRegularFile(join(directory, name), `${label}/${name}`);
}

function readPlatformInput(directory, expectedTarget) {
    const manifestPath = join(directory, "release-artifact.json");
    const manifest = readJson(manifestPath, `${expectedTarget} release-artifact.json`);
    assertExactKeys(manifest, ARTIFACT_KEYS, `${expectedTarget} release-artifact.json`);
    if (manifest.schema !== 1 || manifest.product !== "aster") {
        fail(`${expectedTarget} release artifact has invalid schema or product`);
    }
    if (!SEMVER.test(manifest.version)) {
        fail(`${expectedTarget} release artifact has an invalid version`);
    }
    if (manifest.target !== expectedTarget) {
        fail(`${expectedTarget} release artifact reports target ${manifest.target}`);
    }
    const expectedName = releaseArtifactNames(manifest.version, expectedTarget).archiveName;
    if (manifest.archive !== expectedName) {
        fail(`${expectedTarget} release artifact has an unexpected archive name`);
    }
    assertPlainBasename(manifest.archive, `${expectedTarget} archive`);
    if (!isHexSha256(manifest.sha256)) {
        fail(`${expectedTarget} release artifact has an invalid SHA-256`);
    }
    if (!Number.isSafeInteger(manifest.size) || manifest.size < 0) {
        fail(`${expectedTarget} release artifact has an invalid size`);
    }
    assertExactDirectoryFiles(
        directory,
        expectedInputFiles(manifest),
        `${expectedTarget} release input`,
    );

    const archivePath = join(directory, manifest.archive);
    const archive = readFileSync(archivePath);
    const actualHash = sha256(archive);
    if (archive.length !== manifest.size) fail(`${expectedTarget} archive size does not match`);
    if (actualHash !== manifest.sha256) fail(`${expectedTarget} archive SHA-256 does not match`);
    verifyChecksum(
        archive,
        readFileSync(join(directory, `${manifest.archive}.sha256`), "utf8"),
        manifest.archive,
    );
    return { manifest, archive, hash: actualHash };
}

export function releaseAssetNames(version) {
    if (!SEMVER.test(version)) fail("Release version must be MAJOR.MINOR.PATCH");
    const names = [];
    for (const target of RELEASE_TARGETS) {
        const versioned = releaseArtifactNames(version, target).archiveName;
        const extension = target === "windows-x64" ? ".zip" : ".tar.gz";
        const alias = `aster-${target}${extension}`;
        names.push(versioned, `${versioned}.sha256`, alias, `${alias}.sha256`);
    }
    names.push(...INSTALLER_NAMES, "release-manifest.json");
    return names.sort();
}

export function aggregateManifest(version, inputs) {
    if (!SEMVER.test(version)) fail("Release version must be MAJOR.MINOR.PATCH");
    const assets = [...inputs]
        .sort((left, right) =>
            left.manifest.target < right.manifest.target
                ? -1
                : left.manifest.target > right.manifest.target
                  ? 1
                  : 0,
        )
        .map(({ manifest, hash }) => {
            const extension = manifest.target === "windows-x64" ? ".zip" : ".tar.gz";
            return {
                target: manifest.target,
                archive: manifest.archive,
                alias: `aster-${manifest.target}${extension}`,
                sha256: hash,
                size: manifest.size,
            };
        });
    return {
        schema: RELEASE_MANIFEST_SCHEMA,
        product: "aster",
        version,
        assets,
    };
}

function removeKnownOutput(outputDirectory, expectedNames) {
    if (!existsSync(outputDirectory)) return;
    assertDirectoryNotSymlink(outputDirectory, "release assets output");
    const actual = readdirSync(outputDirectory).sort();
    const unexpected = actual.find((name) => !expectedNames.includes(name));
    if (unexpected) fail(`release assets output contains unexpected file: ${unexpected}`);
    for (const name of actual) {
        const path = join(outputDirectory, name);
        assertRegularFile(path, `release assets output/${name}`);
        rmSync(path);
    }
    rmdirSync(outputDirectory);
}

function copyInstaller(sourceDirectory, destinationDirectory, name) {
    const source = join(sourceDirectory, name);
    assertRegularFile(source, `installer ${name}`);
    const destination = join(destinationDirectory, name);
    copyFileSync(source, destination);
    if (name.endsWith(".sh") && process.platform !== "win32") chmodSync(destination, 0o755);
}

export function validateReleaseAssets({
    repositoryRoot,
    outputDirectory = join(repositoryRoot, "dist", "release-assets"),
} = {}) {
    if (!repositoryRoot) fail("Repository root is required");
    assertFixedDirectory(
        repositoryRoot,
        outputDirectory,
        ["dist", "release-assets"],
        "Release assets output",
    );
    assertDirectoryNotSymlink(outputDirectory, "release assets output");
    const manifest = readJson(
        join(outputDirectory, "release-manifest.json"),
        "release-manifest.json",
    );
    assertExactKeys(manifest, ["schema", "product", "version", "assets"], "release-manifest.json");
    if (
        manifest.schema !== RELEASE_MANIFEST_SCHEMA ||
        manifest.product !== "aster" ||
        !SEMVER.test(manifest.version) ||
        !Array.isArray(manifest.assets) ||
        manifest.assets.length !== 2
    ) {
        fail("release-manifest.json has invalid metadata");
    }
    const expectedNames = releaseAssetNames(manifest.version);
    assertExactDirectoryFiles(outputDirectory, expectedNames, "release assets output");

    const targets = manifest.assets.map((asset) => asset.target);
    if (JSON.stringify(targets) !== JSON.stringify(RELEASE_TARGETS)) {
        fail("release-manifest.json targets must be unique and sorted");
    }
    for (const asset of manifest.assets) {
        assertExactKeys(
            asset,
            ["target", "archive", "alias", "sha256", "size"],
            `${asset.target} aggregate asset`,
        );
        assertPlainBasename(asset.archive, `${asset.target} archive`);
        assertPlainBasename(asset.alias, `${asset.target} alias`);
        const expectedArchive = releaseArtifactNames(manifest.version, asset.target).archiveName;
        const extension = asset.target === "windows-x64" ? ".zip" : ".tar.gz";
        const expectedAlias = `aster-${asset.target}${extension}`;
        if (asset.archive !== expectedArchive || asset.alias !== expectedAlias) {
            fail(`${asset.target} aggregate asset has unexpected filenames`);
        }
        if (!isHexSha256(asset.sha256) || !Number.isSafeInteger(asset.size) || asset.size < 0) {
            fail(`${asset.target} aggregate asset has invalid hash or size`);
        }
        const versioned = readFileSync(join(outputDirectory, asset.archive));
        const alias = readFileSync(join(outputDirectory, asset.alias));
        if (!versioned.equals(alias)) fail(`${asset.target} alias is not byte-for-byte identical`);
        if (versioned.length !== asset.size || sha256(versioned) !== asset.sha256) {
            fail(`${asset.target} aggregate asset does not match archive metadata`);
        }
        verifyChecksum(
            versioned,
            readFileSync(join(outputDirectory, `${asset.archive}.sha256`), "utf8"),
            asset.archive,
        );
        verifyChecksum(
            alias,
            readFileSync(join(outputDirectory, `${asset.alias}.sha256`), "utf8"),
            asset.alias,
        );
    }
    for (const name of INSTALLER_NAMES) assertRegularFile(join(outputDirectory, name), name);
    return manifest;
}

export function prepareReleaseAssets({
    repositoryRoot,
    inputRoot = join(repositoryRoot, "dist", "release-inputs"),
    outputDirectory = join(repositoryRoot, "dist", "release-assets"),
    installersDirectory = join(repositoryRoot, "install"),
} = {}) {
    if (!repositoryRoot) fail("Repository root is required");
    assertFixedDirectory(
        repositoryRoot,
        inputRoot,
        ["dist", "release-inputs"],
        "Release inputs",
    );
    assertFixedDirectory(
        repositoryRoot,
        outputDirectory,
        ["dist", "release-assets"],
        "Release assets output",
    );
    assertFixedDirectory(
        repositoryRoot,
        installersDirectory,
        ["install"],
        "Installers directory",
    );
    assertDirectoryNotSymlink(inputRoot, "release inputs");
    assertDirectoryNotSymlink(installersDirectory, "installers directory");

    const inputDirectories = readdirSync(inputRoot).sort();
    if (JSON.stringify(inputDirectories) !== JSON.stringify(RELEASE_TARGETS)) {
        fail("Release inputs must contain exactly linux-x64 and windows-x64");
    }
    const inputs = RELEASE_TARGETS.map((target) =>
        readPlatformInput(join(inputRoot, target), target),
    );
    const version = inputs[0].manifest.version;
    if (inputs.some((input) => input.manifest.version !== version)) {
        fail("Release input versions do not match");
    }
    const expectedNames = releaseAssetNames(version);
    for (const name of INSTALLER_NAMES) {
        assertRegularFile(join(installersDirectory, name), `installer ${name}`);
    }
    removeKnownOutput(outputDirectory, expectedNames);

    const staging = join(repositoryRoot, "dist", ".release-assets-staging");
    if (existsSync(staging)) fail("Release assets staging directory already exists");
    mkdirSync(staging);
    try {
        for (const input of inputs) {
            const target = input.manifest.target;
            const extension = target === "windows-x64" ? ".zip" : ".tar.gz";
            const alias = `aster-${target}${extension}`;
            writeFileSync(join(staging, input.manifest.archive), input.archive);
            writeFileSync(
                join(staging, `${input.manifest.archive}.sha256`),
                checksumText(input.hash, input.manifest.archive),
            );
            writeFileSync(join(staging, alias), input.archive);
            writeFileSync(join(staging, `${alias}.sha256`), checksumText(input.hash, alias));
        }
        for (const name of INSTALLER_NAMES) copyInstaller(installersDirectory, staging, name);
        const manifest = aggregateManifest(version, inputs);
        writeFileSync(
            join(staging, "release-manifest.json"),
            `${JSON.stringify(manifest, null, 2)}\n`,
        );
        renameSync(staging, outputDirectory);
        return validateReleaseAssets({ repositoryRoot, outputDirectory });
    } catch (error) {
        rmSync(staging, { recursive: true, force: true });
        throw error;
    }
}

const isMain = process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
    try {
        const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
        if (process.argv.length === 2) {
            const manifest = prepareReleaseAssets({ repositoryRoot });
            console.log("\nASTER release assets prepared\n");
            console.log(`Version: ${manifest.version}`);
            console.log(`Assets: ${releaseAssetNames(manifest.version).length}`);
            console.log(`Path: ${join(repositoryRoot, "dist", "release-assets")}`);
        } else if (process.argv.length === 3 && process.argv[2] === "--validate") {
            const manifest = validateReleaseAssets({ repositoryRoot });
            console.log(`ASTER release assets are valid for ${manifest.version}`);
        } else {
            fail("prepare-release-assets accepts only --validate");
        }
    } catch (error) {
        console.error(`error: ${error.message}`);
        process.exit(1);
    }
}
