#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const mode = process.argv[2] ?? "--check";
if (mode !== "--check" && mode !== "--write") {
    console.error("usage: node sync-version.mjs [--check|--write]");
    process.exit(2);
}

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "../../..");
const cargoManifestPath = resolve(repositoryRoot, "Cargo.toml");
const extensionManifestPath = resolve(repositoryRoot, "editors/vscode/package.json");
const extensionLockPath = resolve(repositoryRoot, "editors/vscode/package-lock.json");

function readWorkspaceVersion() {
    const lines = readFileSync(cargoManifestPath, "utf8").split(/\r?\n/);
    let inWorkspacePackage = false;

    for (const line of lines) {
        const section = line.match(/^\s*\[([^\]]+)]\s*$/);
        if (section) {
            inWorkspacePackage = section[1] === "workspace.package";
            continue;
        }

        if (inWorkspacePackage) {
            const version = line.match(/^\s*version\s*=\s*"([^"]+)"\s*$/);
            if (version) {
                return version[1];
            }
        }
    }

    throw new Error("[workspace.package].version was not found in Cargo.toml");
}

function readJson(path) {
    return JSON.parse(readFileSync(path, "utf8"));
}

function writeJson(path, value) {
    writeFileSync(path, `${JSON.stringify(value, null, 4)}\n`, "utf8");
}

const workspaceVersion = readWorkspaceVersion();
const extensionManifest = readJson(extensionManifestPath);
const extensionLock = readJson(extensionLockPath);
const lockRoot = extensionLock.packages?.[""];

if (!lockRoot) {
    throw new Error('package-lock.json does not contain the root package at packages[""]');
}

const mismatches = [];
if (extensionManifest.version !== workspaceVersion) {
    mismatches.push(`package.json=${extensionManifest.version}`);
}
if (extensionLock.version !== workspaceVersion) {
    mismatches.push(`package-lock.json=${extensionLock.version}`);
}
if (lockRoot.version !== workspaceVersion) {
    mismatches.push(`package-lock.json packages[""]=${lockRoot.version}`);
}

if (mode === "--check") {
    if (mismatches.length > 0) {
        console.error(
            `release version mismatch: Cargo.toml=${workspaceVersion}; ${mismatches.join("; ")}`,
        );
        console.error("run: node editors/vscode/scripts/sync-version.mjs --write");
        process.exit(1);
    }

    console.log(`release versions agree at ${workspaceVersion}`);
    process.exit(0);
}

extensionManifest.version = workspaceVersion;
extensionLock.version = workspaceVersion;
lockRoot.version = workspaceVersion;
writeJson(extensionManifestPath, extensionManifest);
writeJson(extensionLockPath, extensionLock);
console.log(`synchronized VS Code extension manifests to ${workspaceVersion}`);
