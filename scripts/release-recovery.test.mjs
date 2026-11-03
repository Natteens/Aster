#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { classifyReleaseRecovery, highestReleaseTag } from "./resolve-release-recovery.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const recoveryWorkflow = readFileSync(join(root, ".github", "workflows", "release-recovery.yml"), "utf8");
const releaseWorkflow = readFileSync(join(root, ".github", "workflows", "release.yml"), "utf8");
const recoveryScript = readFileSync(join(root, "scripts", "resolve-release-recovery.mjs"), "utf8");

const sha = (character) => character.repeat(40);

/** An interrupted release: release commit and tag exist on main, public release does not. */
const interrupted = {
    tag: "v0.67.0",
    dispatchRef: "refs/heads/main",
    headSha: sha("a"),
    remoteMainSha: sha("a"),
    tagCommitSha: sha("a"),
    localTagObjectSha: sha("a"),
    remoteTagObjectSha: sha("a"),
    versionAtTag: "0.67.0",
    commitSubject: "chore(release): 0.67.0 [skip ci]",
    tagOnMain: true,
    highestTag: "v0.67.0",
    releaseExists: false,
};

test("an interrupted release with a verified tag is recoverable for that exact commit", () => {
    assert.deepEqual(classifyReleaseRecovery(interrupted), {
        version: "0.67.0",
        tag: "v0.67.0",
        sha: sha("a"),
    });
});

test("a recoverable tag may be older than main when it is the newest release", () => {
    // Later non-release commits on main must not block recovering the newest tag.
    assert.deepEqual(
        classifyReleaseRecovery({ ...interrupted, headSha: sha("c"), remoteMainSha: sha("c") }),
        { version: "0.67.0", tag: "v0.67.0", sha: sha("a") },
    );
});

test("recovery fails closed when the tag does not match the commit or version", () => {
    const rejected = [
        [{ tag: "0.67.0" }, /not a release tag/],
        [{ tag: "v0.67.0-rc.1" }, /not a release tag/],
        [{ tag: "v0.67.0; rm -rf" }, /not a release tag/],
        [{ versionAtTag: "0.66.6" }, /does not match/],
        [{ tag: "v0.67.1", highestTag: "v0.67.1" }, /does not match/],
        [{ commitSubject: "feat: sneak a feature in" }, /canonical release commit/],
        [{ commitSubject: "chore(release): 0.66.6 [skip ci]" }, /canonical release commit/],
        [{ localTagObjectSha: sha("d") }, /local and remote/],
        [{ remoteTagObjectSha: sha("d"), localTagObjectSha: sha("a") }, /local and remote/],
        [{ tagCommitSha: "not-a-sha" }, /commit identifier/],
    ];
    for (const [override, message] of rejected) {
        assert.throws(() => classifyReleaseRecovery({ ...interrupted, ...override }), message, JSON.stringify(override));
    }
});

test("recovery rejects commits outside origin/main and dispatches from other refs", () => {
    assert.throws(() => classifyReleaseRecovery({ ...interrupted, tagOnMain: false }), /origin\/main/);
    assert.throws(
        () => classifyReleaseRecovery({ ...interrupted, dispatchRef: "refs/heads/feature" }),
        /main branch/,
    );
    assert.throws(
        () => classifyReleaseRecovery({ ...interrupted, dispatchRef: "refs/tags/v0.67.0" }),
        /main branch/,
    );
    assert.throws(
        () => classifyReleaseRecovery({ ...interrupted, headSha: sha("b") }),
        /current origin\/main/,
    );
});

test("recovery never substitutes another release or overwrites a published one", () => {
    assert.throws(
        () => classifyReleaseRecovery({ ...interrupted, highestTag: "v0.68.0" }),
        /newest release tag/,
    );
    assert.throws(() => classifyReleaseRecovery({ ...interrupted, releaseExists: true }), /already published/);
    assert.throws(() => classifyReleaseRecovery({ ...interrupted, releaseExists: undefined }), /could not be determined/);
});

test("the newest release tag is chosen by numeric version, not text order", () => {
    assert.equal(highestReleaseTag(["v0.9.0", "v0.10.0", "v0.67.0", "v0.66.6", "v0.100.0", "junk", "v1.0.0-rc.1"]), "v0.100.0");
    assert.equal(highestReleaseTag(["v0.66.6", "v0.67.0"]), "v0.67.0");
    assert.equal(highestReleaseTag(["junk"]), "");
});

test("the recovery workflow is a separate, input-bound, main-only entry point", () => {
    assert.match(recoveryWorkflow, /workflow_dispatch:\s*\n\s+inputs:\s*\n\s+release_tag:\s*\n[\s\S]*?required: true/);
    assert.match(recoveryWorkflow, /^permissions:\s*\n\s+contents: read/m);
    assert.match(recoveryWorkflow, /github\.ref == 'refs\/heads\/main'/);
    assert.doesNotMatch(recoveryWorkflow, /^\s+(push|pull_request|schedule):/m);
    assert.match(recoveryWorkflow, /ASTER_RECOVERY_TAG: \$\{\{ inputs\.release_tag \}\}/);
    // The tag input is only ever read through an environment variable, never interpolated into a shell.
    const runBlocks = recoveryWorkflow.split(/\n\s+run: /).slice(1).join("\n");
    assert.equal(runBlocks.includes("inputs.release_tag"), false);
});

test("recovery reuses the validated release pipeline and never runs semantic-release", () => {
    assert.match(recoveryWorkflow, /uses: \.\/\.github\/workflows\/release\.yml/);
    for (const field of ["version", "release_tag", "release_sha"]) {
        assert.match(recoveryWorkflow, new RegExp(`${field}: \\$\\{\\{ needs\\.resolve\\.outputs\\.${field} \\}\\}`));
    }
    assert.match(recoveryWorkflow, /release-pipeline:[\s\S]*?permissions:\s*\n\s+contents: write/);
    assert.equal(recoveryWorkflow.includes("gh release create"), false);
    for (const forbidden of [/semantic-release/, /npm run release/, /git push/, /git tag/, /sync-release-version/]) {
        assert.doesNotMatch(recoveryWorkflow, forbidden);
        assert.doesNotMatch(recoveryScript, forbidden);
    }
});

test("manual runs of the release workflow itself still cannot publish", () => {
    assert.match(releaseWorkflow, /workflow_dispatch:\s*\n\s*\npermissions:/);
    assert.match(
        releaseWorkflow,
        /if: >-\s*\n\s+needs\.validate\.outputs\.is_release_tag == 'true' &&\s*\n\s+inputs\.release_sha != ''/,
    );
    assert.equal((releaseWorkflow.match(/gh release create/g) ?? []).length, 1);
});
