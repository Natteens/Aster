#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version)) {
    console.error("usage: node scripts/sync-release-version.mjs VERSION");
    process.exit(2);
}

const repositoryRoot = resolve(import.meta.dirname, "..");
const cargoManifestPath = resolve(repositoryRoot, "Cargo.toml");
const cratesDirectory = resolve(repositoryRoot, "crates");
const extensionSyncScript = resolve(
    repositoryRoot,
    "editors/vscode/scripts/sync-version.mjs",
);

function replaceOne(source, expression, replacement, description) {
    if (!expression.test(source)) {
        throw new Error(`${description} was not found`);
    }
    return source.replace(expression, replacement);
}

function writeIfChanged(path, source) {
    const current = readFileSync(path, "utf8");
    if (current !== source) {
        writeFileSync(path, source, "utf8");
    }
}

const rootManifest = readFileSync(cargoManifestPath, "utf8");
const updatedRootManifest = replaceOne(
    rootManifest,
    /(\[workspace\.package\][\s\S]*?^\s*version\s*=\s*)"[^"]+"/m,
    `$1"${version}"`,
    "[workspace.package].version",
);
writeIfChanged(cargoManifestPath, updatedRootManifest);

const localPackageNames = [];
for (const entry of readdirSync(cratesDirectory, { withFileTypes: true })) {
    if (!entry.isDirectory()) {
        continue;
    }
    const manifestPath = resolve(cratesDirectory, entry.name, "Cargo.toml");
    let manifest;
    try {
        manifest = readFileSync(manifestPath, "utf8");
    } catch (error) {
        if (error?.code === "ENOENT") {
            continue;
        }
        throw error;
    }

    const packageName = readPackageName(manifest, manifestPath);
    localPackageNames.push(packageName);

    const updatedManifest = manifest.replace(
        /(aster-[\w-]+\s*=\s*\{[^}\n]*\bversion\s*=\s*)"[^"]+"/g,
        `$1"${version}"`,
    );
    writeIfChanged(manifestPath, updatedManifest);
}

execFileSync(process.execPath, [extensionSyncScript, "--write"], {
    cwd: repositoryRoot,
    stdio: "inherit",
});

// Ask Cargo to re-resolve the workspace so Cargo.lock picks up the new local
// package versions. `--no-deps` must NOT be used here: it makes `cargo
// metadata` skip lock-file resolution entirely, so the lock keeps recording
// the previous versions for every workspace member even though the manifests
// were just rewritten above. Without `--no-deps`, Cargo still resolves
// strictly against the existing lock entries for every external dependency,
// so nothing outside the workspace is upgraded.
execFileSync("cargo", ["metadata", "--format-version", "1"], {
    cwd: repositoryRoot,
    stdio: ["ignore", "ignore", "inherit"],
});

verifyLockfileVersions(resolve(repositoryRoot, "Cargo.lock"), localPackageNames, version);

function readPackageName(manifest, manifestPath) {
    const lines = manifest.split(/\r?\n/);
    let inPackageSection = false;
    for (const line of lines) {
        const section = line.match(/^\s*\[([^\]]+)]\s*$/);
        if (section) {
            inPackageSection = section[1] === "package";
            continue;
        }
        if (inPackageSection) {
            const name = line.match(/^\s*name\s*=\s*"([^"]+)"\s*$/);
            if (name) {
                return name[1];
            }
        }
    }
    throw new Error(`[package].name was not found in ${manifestPath}`);
}

function verifyLockfileVersions(lockfilePath, packageNames, expectedVersion) {
    const lockfile = readFileSync(lockfilePath, "utf8");
    const versionsByName = new Map();
    for (const block of lockfile.split("[[package]]")) {
        const name = block.match(/^\s*name\s*=\s*"([^"]+)"\s*$/m)?.[1];
        const packageVersion = block.match(/^\s*version\s*=\s*"([^"]+)"\s*$/m)?.[1];
        if (name && packageVersion) {
            versionsByName.set(name, packageVersion);
        }
    }

    const stale = packageNames.filter((name) => versionsByName.get(name) !== expectedVersion);
    if (stale.length > 0) {
        throw new Error(
            `Cargo.lock is out of sync with version ${expectedVersion} for: ${stale.join(", ")}`,
        );
    }
}
