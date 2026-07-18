#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { cpSync, mkdtempSync, mkdirSync, readFileSync, rmSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function createFixture() {
    const fixtureRoot = mkdtempSync(join(tmpdir(), "aster-sync-release-version-"));

    cpSync(resolve(repositoryRoot, "Cargo.toml"), resolve(fixtureRoot, "Cargo.toml"));
    cpSync(resolve(repositoryRoot, "Cargo.lock"), resolve(fixtureRoot, "Cargo.lock"));
    cpSync(resolve(repositoryRoot, "crates"), resolve(fixtureRoot, "crates"), { recursive: true });
    cpSync(resolve(repositoryRoot, "stdlib"), resolve(fixtureRoot, "stdlib"), { recursive: true });
    cpSync(
        resolve(repositoryRoot, "scripts/sync-release-version.mjs"),
        resolve(fixtureRoot, "scripts/sync-release-version.mjs"),
    );

    mkdirSync(resolve(fixtureRoot, "editors/vscode/scripts"), { recursive: true });
    cpSync(
        resolve(repositoryRoot, "editors/vscode/package.json"),
        resolve(fixtureRoot, "editors/vscode/package.json"),
    );
    cpSync(
        resolve(repositoryRoot, "editors/vscode/package-lock.json"),
        resolve(fixtureRoot, "editors/vscode/package-lock.json"),
    );
    cpSync(
        resolve(repositoryRoot, "editors/vscode/scripts/sync-version.mjs"),
        resolve(fixtureRoot, "editors/vscode/scripts/sync-version.mjs"),
    );

    return fixtureRoot;
}

function localPackageNames(fixtureRoot) {
    const cratesDirectory = resolve(fixtureRoot, "crates");
    const names = [];
    for (const entry of readdirSync(cratesDirectory, { withFileTypes: true })) {
        if (!entry.isDirectory()) {
            continue;
        }
        const manifest = readFileSync(resolve(cratesDirectory, entry.name, "Cargo.toml"), "utf8");
        const name = manifest.match(/^\s*name\s*=\s*"([^"]+)"\s*$/m)?.[1];
        assert.ok(name, `expected a [package].name in crates/${entry.name}/Cargo.toml`);
        names.push(name);
    }
    return names;
}

function lockedPackageVersions(fixtureRoot) {
    const lockfile = readFileSync(resolve(fixtureRoot, "Cargo.lock"), "utf8");
    const versionsByName = new Map();
    for (const block of lockfile.split("[[package]]")) {
        const name = block.match(/^\s*name\s*=\s*"([^"]+)"\s*$/m)?.[1];
        const version = block.match(/^\s*version\s*=\s*"([^"]+)"\s*$/m)?.[1];
        if (name && version) {
            versionsByName.set(name, version);
        }
    }
    return versionsByName;
}

function runSync(fixtureRoot, version) {
    execFileSync(process.execPath, ["scripts/sync-release-version.mjs", version], {
        cwd: fixtureRoot,
        stdio: "pipe",
    });
}

test("sync-release-version updates the workspace, Cargo.lock, and VS Code manifests", () => {
    const fixtureRoot = createFixture();
    try {
        const names = localPackageNames(fixtureRoot);
        const versionsBefore = lockedPackageVersions(fixtureRoot);
        const externalVersionsBefore = new Map(
            [...versionsBefore].filter(([name]) => !names.includes(name)),
        );

        const nextVersion = "0.2.0";
        runSync(fixtureRoot, nextVersion);

        const workspaceManifest = readFileSync(resolve(fixtureRoot, "Cargo.toml"), "utf8");
        assert.match(
            workspaceManifest,
            new RegExp(`\\[workspace\\.package\\][\\s\\S]*?version = "${nextVersion}"`),
        );

        const versionsAfter = lockedPackageVersions(fixtureRoot);
        for (const name of names) {
            assert.equal(
                versionsAfter.get(name),
                nextVersion,
                `expected Cargo.lock entry for ${name} to be ${nextVersion}`,
            );
        }

        for (const [name, version] of externalVersionsBefore) {
            assert.equal(
                versionsAfter.get(name),
                version,
                `external dependency ${name} must not change version`,
            );
        }
        assert.equal(
            versionsAfter.size,
            versionsBefore.size,
            "no packages should be added to or removed from Cargo.lock",
        );

        const extensionManifest = JSON.parse(
            readFileSync(resolve(fixtureRoot, "editors/vscode/package.json"), "utf8"),
        );
        assert.equal(extensionManifest.version, nextVersion);
        const extensionLock = JSON.parse(
            readFileSync(resolve(fixtureRoot, "editors/vscode/package-lock.json"), "utf8"),
        );
        assert.equal(extensionLock.version, nextVersion);
        assert.equal(extensionLock.packages?.[""]?.version, nextVersion);

        // Second run at the same version must be a no-op (idempotent).
        const lockAfterFirstRun = readFileSync(resolve(fixtureRoot, "Cargo.lock"), "utf8");
        const manifestAfterFirstRun = readFileSync(resolve(fixtureRoot, "Cargo.toml"), "utf8");
        runSync(fixtureRoot, nextVersion);
        assert.equal(readFileSync(resolve(fixtureRoot, "Cargo.lock"), "utf8"), lockAfterFirstRun);
        assert.equal(readFileSync(resolve(fixtureRoot, "Cargo.toml"), "utf8"), manifestAfterFirstRun);

        // Best-effort: confirm the resulting lockfile satisfies --locked, when Cargo is available offline.
        try {
            execFileSync("cargo", ["check", "--workspace", "--locked"], {
                cwd: fixtureRoot,
                stdio: "pipe",
            });
        } catch (error) {
            if (error.code === "ENOENT") {
                return;
            }
            throw error;
        }
    } finally {
        rmSync(fixtureRoot, { recursive: true, force: true });
    }
});
