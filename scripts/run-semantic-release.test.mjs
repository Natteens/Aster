#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";

import { classifyReleaseRun } from "./run-semantic-release.mjs";

const url = "https://github.com/Natteens/Aster";

function notesFailure(version, { ref = `refs/notes/semantic-release-v${version}` } = {}) {
    return [
        "[8:07:26 PM] [semantic-release] › ✔  Completed step \"generateNotes\" of plugin \"@semantic-release/exec\"",
        `[8:07:28 PM] [semantic-release] › ✘  An error occurred while running semantic-release: ExecaError: Command failed with exit code 1: git push -- '${url}' ${ref}`,
        "",
        `To ${url}`,
        ` ! [remote rejected] ${ref} -> ${ref} (failed)`,
        `error: failed to push some refs to '${url}'`,
        "",
    ].join("\n");
}

const sound = { beforeVersion: "0.66.6", afterVersion: "0.67.0", integrityError: null };

test("a normal release handoff needs no special handling", () => {
    assert.deepEqual(classifyReleaseRun({ ...sound, exitCode: 0, output: "Published release" }), {
        outcome: "success",
    });
    // A run that finds nothing to release is still an ordinary success.
    assert.deepEqual(
        classifyReleaseRun({ ...sound, afterVersion: "0.66.6", exitCode: 0, output: "" }),
        { outcome: "success" },
    );
});

test("a rejected Git Notes push after a verified release is tolerated", () => {
    assert.deepEqual(classifyReleaseRun({ ...sound, exitCode: 1, output: notesFailure("0.67.0") }), {
        outcome: "notes-only",
        notesRef: "refs/notes/semantic-release-v0.67.0",
    });
});

test("the tolerance is bound to the exact release version and notes ref", () => {
    for (const output of [
        notesFailure("0.67.0", { ref: "refs/notes/semantic-release-v0.66.6" }),
        notesFailure("0.67.0", { ref: "refs/notes/semantic-release-v0.67.1" }),
        notesFailure("0.67.0", { ref: "refs/notes/other" }),
        notesFailure("0.67.0", { ref: "refs/heads/main" }),
        notesFailure("0.67.0", { ref: "refs/tags/v0.67.0" }),
    ]) {
        assert.throws(() => classifyReleaseRun({ ...sound, exitCode: 1, output }), /semantic-release failed/);
    }
});

test("an unverified release never downgrades a notes failure", () => {
    assert.throws(
        () =>
            classifyReleaseRun({
                ...sound,
                exitCode: 1,
                output: notesFailure("0.67.0"),
                integrityError: "release tag, release commit, and remote main do not agree",
            }),
        /do not agree/,
    );
});

test("a notes failure without a created release is not tolerated", () => {
    assert.throws(
        () =>
            classifyReleaseRun({
                ...sound,
                afterVersion: "0.66.6",
                exitCode: 1,
                output: notesFailure("0.66.6"),
            }),
        /semantic-release failed/,
    );
});

test("unexpected Git, publication, and plugin errors propagate", () => {
    const failures = [
        `[semantic-release] › ✘  An error occurred while running semantic-release: ExecaError: Command failed with exit code 1: git push -- '${url}' HEAD:refs/heads/main`,
        `[semantic-release] › ✘  An error occurred while running semantic-release: ExecaError: Command failed with exit code 1: git push --tags -- '${url}'`,
        "[semantic-release] › ✘  An error occurred while running semantic-release: Error: EGITNOPERMISSION",
        "[semantic-release] › ✘  Failed step \"prepare\" of plugin \"@semantic-release/git\"",
        "",
    ];
    for (const output of failures) {
        assert.throws(
            () => classifyReleaseRun({ ...sound, exitCode: 1, output }),
            /semantic-release failed/,
            output,
        );
    }
});

test("a second error alongside the notes failure is not tolerated", () => {
    const output = `${notesFailure("0.67.0")}\n[semantic-release] › ✘  An error occurred while running semantic-release: Error: second failure\n`;
    assert.throws(() => classifyReleaseRun({ ...sound, exitCode: 1, output }), /semantic-release failed/);
});

test("the release never rewrites the version it was given", () => {
    // A successful run that changed nothing must not look like a bump.
    assert.deepEqual(
        classifyReleaseRun({
            beforeVersion: "0.67.0",
            afterVersion: "0.67.0",
            exitCode: 0,
            output: "",
            integrityError: null,
        }),
        { outcome: "success" },
    );
});
