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
