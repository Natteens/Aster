#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";

import { validateReleaseContext } from "./validate-release-ref.mjs";

test("manual release validation uses the canonical version without publishing", () => {
    assert.deepEqual(
        validateReleaseContext({
            version: "2.3.4",
            eventName: "workflow_dispatch",
            checkedOutSha: "1".repeat(40),
        }),
        {
            version: "2.3.4",
            is_release_tag: "false",
            release_sha: "1".repeat(40),
        },
    );
});

test("a validated semantic-release workflow call is publishable", () => {
    const sha = "1".repeat(40);
    assert.deepEqual(
        validateReleaseContext({
            version: "2.3.4",
            eventName: "workflow_call",
            releaseVersion: "2.3.4",
            releaseTag: "v2.3.4",
            releaseSha: sha,
            checkedOutSha: sha,
            tagSha: sha,
            commitOnMain: true,
        }),
        { version: "2.3.4", is_release_tag: "true", release_sha: sha },
    );
});

test("invalid tags, divergent versions and SHAs, and off-main commits are rejected", () => {
    const sha = "1".repeat(40);
    for (const [overrides, pattern] of [
        [{ releaseTag: "v2.3" }, /not valid/],
        [{ releaseTag: "v2.3.4-beta" }, /not valid/],
        [{ releaseTag: "release-2.3.4" }, /not valid/],
        [{ releaseTag: "v02.3.4" }, /not valid/],
        [{ releaseVersion: "2.3.5", releaseTag: "v2.3.5" }, /does not match/],
        [{ tagSha: "2".repeat(40) }, /do not agree/],
        [{ checkedOutSha: "2".repeat(40) }, /do not agree/],
        [{ commitOnMain: false }, /not in the origin\/main history/],
    ]) {
        assert.throws(
            () =>
                validateReleaseContext({
                    version: "2.3.4",
                    eventName: "workflow_call",
                    releaseVersion: "2.3.4",
                    releaseTag: "v2.3.4",
                    releaseSha: sha,
                    checkedOutSha: sha,
                    tagSha: sha,
                    commitOnMain: true,
                    ...overrides,
                }),
            pattern,
        );
    }
});

test("canonical prerelease and leading-zero versions are rejected", () => {
    for (const version of ["2.3", "2.3.4-beta", "02.3.4"]) {
        assert.throws(
            () =>
                validateReleaseContext({
                    version,
                    eventName: "workflow_dispatch",
                    checkedOutSha: "1".repeat(40),
                }),
            /Canonical version/,
        );
    }
});

test("manual runs cannot provide publication data or emulate an automatic release", () => {
    assert.throws(
        () =>
            validateReleaseContext({
                version: "2.3.4",
                eventName: "workflow_dispatch",
                releaseVersion: "2.3.4",
                releaseTag: "v2.3.4",
                releaseSha: "1".repeat(40),
                checkedOutSha: "1".repeat(40),
            }),
        /does not accept publication inputs/,
    );
});
