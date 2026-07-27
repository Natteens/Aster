#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const workflow = readFileSync(join(root, ".github", "workflows", "release.yml"), "utf8");
const ci = readFileSync(join(root, ".github", "workflows", "ci.yml"), "utf8");
const releaseConfig = readFileSync(join(root, "release.config.mjs"), "utf8");
const packageManifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const windowsInstaller = readFileSync(join(root, "install", "install.ps1"), "utf8");
const linuxInstaller = readFileSync(join(root, "install", "install.sh"), "utf8");

const actionPins = new Map([
    ["actions/checkout", "3d3c42e5aac5ba805825da76410c181273ba90b1"],
    ["actions/setup-node", "820762786026740c76f36085b0efc47a31fe5020"],
    ["actions/upload-artifact", "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"],
    ["actions/download-artifact", "3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"],
    ["actions/cache", "55cc8345863c7cc4c66a329aec7e433d2d1c52a9"],
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

test("release workflow is reusable for validated releases and manual runs have no inputs", () => {
    assert.match(workflow, /workflow_call:\s*\n\s+inputs:/);
    for (const input of ["version", "release_tag", "release_sha"]) {
        assert.match(workflow, new RegExp(`${input}:\\s*\\n\\s+required: true`));
    }
    assert.match(workflow, /workflow_dispatch:\s*\n\s*\npermissions:/);
    assert.doesNotMatch(workflow, /^\s+push:/m);
    assert.match(workflow, /group: release-\$\{\{ github\.ref \}\}/);
    assert.match(workflow, /cancel-in-progress: false/);
});

test("release workflow uses least privilege and publish is workflow-call-only", () => {
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
        /if: github\.event_name == 'workflow_call' && needs\.validate\.outputs\.is_release_tag == 'true'/,
    );
});

test("main CI runs semantic-release after verification and directly calls M6F", () => {
    assert.match(ci, /^  semantic-release:\s*$/m);
    assert.match(ci, /semantic-release:[\s\S]*needs: verify/);
    assert.match(ci, /github\.event_name == 'push'/);
    assert.match(ci, /github\.ref == 'refs\/heads\/main'/);
    assert.match(ci, /!contains\(github\.event\.head_commit\.message, '\[skip ci\]'\)/);
    assert.match(ci, /run: npm run release/);
    assert.match(ci, /^  release-pipeline:\s*$/m);
    assert.match(ci, /uses: \.\/\.github\/workflows\/release\.yml/);
    assert.match(ci, /if: needs\.semantic-release\.outputs\.released == 'true'/);
    for (const output of ["version", "release_tag", "release_sha"]) {
        assert.match(
            ci,
            new RegExp(`${output}: \\$\\{\\{ needs\\.semantic-release\\.outputs\\.${output} \\}\\}`),
        );
    }
});

test("semantic-release owns version and tag but not the GitHub Release", () => {
    assert.match(releaseConfig, /@semantic-release\/commit-analyzer/);
    assert.match(releaseConfig, /@semantic-release\/changelog/);
    assert.match(releaseConfig, /@semantic-release\/git/);
    assert.match(releaseConfig, /chore\(release\): \$\{nextRelease\.version\} \[skip ci\]/);
    assert.doesNotMatch(releaseConfig, /@semantic-release\/github/);
    assert.equal((workflow.match(/gh release create/g) ?? []).length, 1);
    assert.equal((ci.match(/gh release create/g) ?? []).length, 0);
});

test("every release-path action is official and pinned to its audited commit", () => {
    const releasePath = `${ci}\n${workflow}`;
    const uses = [...releasePath.matchAll(/uses:\s+([^@\s]+)@([0-9a-f]+)(?:\s+#.*)?/g)];
    assert.ok(uses.length > 0);
    for (const [, name, revision] of uses) {
        assert.equal(actionPins.get(name), revision, `${name}@${revision}`);
        assert.match(revision, /^[0-9a-f]{40}$/);
    }
    assert.equal((releasePath.match(/\buses:/g) ?? []).length, uses.length + 1);
    assert.match(releasePath, /uses: \.\/\.github\/workflows\/release\.yml/);
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

test("quality gates run once before release builds and installer lifecycle runs only on final assets", () => {
    for (const command of [
        "cargo fmt --all --check",
        "cargo clippy --workspace --all-targets --all-features -- -D warnings",
        "cargo test --workspace --all-targets",
        "cargo check --workspace --locked",
        "npm run test:release-core",
    ]) {
        assert.equal((ci.match(new RegExp(command.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "g")) ?? []).length, 1);
        assert.equal(workflow.includes(command), false, command);
    }
    assert.equal((workflow.match(/npm run test:installers/g) ?? []).length, 1);
    assert.doesNotMatch(workflow.slice(workflow.indexOf("  assemble:"), workflow.indexOf("  verify-assets:")), /install-linux\.test|test:installers/);
    assert.doesNotMatch(releaseConfig, /cargo check --workspace --locked/);
});

test("release core excludes installer lifecycle and the local aggregate remains explicit", () => {
    const core = packageManifest.scripts["test:release-core"];
    assert.match(core, /bundle\.test\.mjs/);
    assert.match(core, /package-release\.test\.mjs/);
    assert.match(core, /release-workflow\.test\.mjs/);
    assert.doesNotMatch(core, /install(?:-linux)?\.test\.mjs/);
    assert.equal(
        packageManifest.scripts["test:release-script"],
        "npm run test:release-core && npm run test:installers",
    );
});

test("Cargo caches are official, Node 24 compatible, and isolated by OS and toolchain", () => {
    for (const source of [ci, workflow]) {
        assert.match(source, /actions\/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6\.1\.0/);
        assert.match(source, /~\/\.cargo\/registry/);
        assert.match(source, /~\/\.cargo\/git/);
        assert.match(source, /\n\s+target\n/);
        assert.match(source, /\$\{\{ runner\.os \}\}/);
        assert.match(source, /hashFiles\('Cargo\.lock'\)/);
        assert.doesNotMatch(source, /dist\/artifacts[\s\S]*actions\/cache|actions\/cache[\s\S]*install-state/);
    }
    assert.match(workflow, /\$\{\{ matrix\.toolchain \}\}/);
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
