#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const workflow = readFileSync(join(root, ".github", "workflows", "release.yml"), "utf8");
const ci = readFileSync(join(root, ".github", "workflows", "ci.yml"), "utf8");
const windowsInstaller = readFileSync(join(root, "install", "install.ps1"), "utf8");
const linuxInstaller = readFileSync(join(root, "install", "install.sh"), "utf8");

const actionPins = new Map([
    ["actions/checkout", "34e114876b0b11c390a56381ad16ebd13914f8d5"],
    ["actions/setup-node", "48b55a011bda9f5d6aeb4c2d9c7362e8dae4041e"],
    ["actions/upload-artifact", "ea165f8d65b6e75b540449e92b4886f43607fa02"],
    ["actions/download-artifact", "d3f86a106a0bac45b974a628896c90dbdf5c8093"],
]);

const fixedAssets = [
    "aster-$VERSION-windows-x64.zip",
    "aster-$VERSION-windows-x64.zip.sha256",
    "aster-windows-x64.zip",
    "aster-windows-x64.zip.sha256",
    "aster-$VERSION-linux-x64.tar.gz",
    "aster-$VERSION-linux-x64.tar.gz.sha256",
    "aster-linux-x64.tar.gz",
    "aster-linux-x64.tar.gz.sha256",
    "install.ps1",
    "install.sh",
    "uninstall.ps1",
    "uninstall.sh",
    "release-manifest.json",
];

test("release workflow has tag and manual triggers with non-cancelling concurrency", () => {
    assert.match(workflow, /push:\s*\n\s+tags:\s*\n\s+- "v\*\.\*\.\*"/);
    assert.match(workflow, /workflow_dispatch:/);
    assert.match(workflow, /group: release-\$\{\{ github\.ref \}\}/);
    assert.match(workflow, /cancel-in-progress: false/);
});

test("release workflow uses least privilege and publish is tag-only", () => {
    assert.match(workflow, /^permissions:\s*\n\s+contents: read/m);
    assert.match(workflow, /publish:[\s\S]*?permissions:\s*\n\s+contents: write/);
    for (const forbidden of [
        "actions: write",
        "packages: write",
        "id-token: write",
        "issues: write",
        "pull-requests: write",
        "security-events: write",
    ]) {
        assert.equal(workflow.includes(forbidden), false, forbidden);
    }
    assert.equal(workflow.includes("secrets."), false);
    assert.match(
        workflow,
        /if: github\.event_name == 'push' && startsWith\(github\.ref, 'refs\/tags\/v'\) && needs\.validate\.outputs\.is_release_tag == 'true'/,
    );
    assert.equal(ci.includes("npm run release"), false);
});

test("every workflow action is official and pinned to its audited commit", () => {
    const uses = [...workflow.matchAll(/uses:\s+([^@\s]+)@([0-9a-f]+)(?:\s+#.*)?/g)];
    assert.ok(uses.length > 0);
    for (const [, name, revision] of uses) {
        assert.equal(actionPins.get(name), revision, `${name}@${revision}`);
        assert.match(revision, /^[0-9a-f]{40}$/);
    }
    assert.equal((workflow.match(/\buses:/g) ?? []).length, uses.length);
});

test("workflow contains bounded build, assembly, final verification, and publish jobs", () => {
    for (const job of ["validate:", "build:", "assemble:", "verify-assets:", "publish:"]) {
        assert.match(workflow, new RegExp(`^  ${job}`, "m"));
    }
    for (const timeout of [10, 60, 15]) {
        assert.match(workflow, new RegExp(`timeout-minutes: ${timeout}`));
    }
    assert.match(workflow, /runner: windows-latest/);
    assert.match(workflow, /runner: ubuntu-latest/);
    assert.match(workflow, /timeout-minutes: 30/);
    assert.match(workflow, /rustup toolchain install stable-gnu --profile minimal/);
    assert.match(workflow, /rustup toolchain install stable --profile minimal/);
    assert.match(workflow, /npm run package:release[\s\S]*npm run package:release/);
    assert.match(
        workflow,
        /ASTER_RELEASE_ASSETS_DIR: dist\/release-assets[\s\S]*npm run test:installers/,
    );
});

test("publish step names exactly the thirteen public assets", () => {
    const publish = workflow.slice(workflow.indexOf("  publish:"));
    for (const asset of fixedAssets) {
        assert.match(publish, new RegExp(asset.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    }
    assert.equal((publish.match(/"dist\/release-assets\//g) ?? []).length, 13);
    assert.match(publish, /gh release create[\s\S]*--draft[\s\S]*gh release upload/);
    assert.match(publish, /gh release edit[\s\S]*--draft=false/);
});

test("installers use the stable latest-release URL and target aliases", () => {
    for (const installer of [windowsInstaller, linuxInstaller]) {
        assert.match(
            installer,
            /https:\/\/github\.com\/Natteens\/Aster\/releases\/latest\/download/,
        );
    }
    assert.match(windowsInstaller, /aster-windows-x64\.zip/);
    assert.match(linuxInstaller, /aster-linux-x64\.tar\.gz/);
});
