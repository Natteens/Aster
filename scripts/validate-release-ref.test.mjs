#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";

import { validateReleaseContext } from "./validate-release-ref.mjs";

const RELEASE_SHA = "c7dbf01b76552c293a9d71b5b387b815d7db77d7";
const CALLER_SHA = "9a4a36c0af278681c81ab70a6d404847e7acd40c";

test("manual release validation uses the canonical version without publishing", () => {
    assert.deepEqual(
        validateReleaseContext({
            version: "2.3.4",
            mode: "manual",
            checkedOutSha: "1".repeat(40),
        }),
        {
            version: "2.3.4",
            is_release_tag: "false",
            release_sha: "1".repeat(40),
        },
    );
});

test("automatic mode accepts a release created by a caller push event", () => {
    assert.deepEqual(
        validateReleaseContext({
            version: "0.48.0",
            mode: "automatic",
            eventName: "push",
            releaseVersion: "0.48.0",
            releaseTag: "v0.48.0",
            releaseSha: RELEASE_SHA,
            checkedOutSha: RELEASE_SHA,
            tagSha: RELEASE_SHA,
            commitOnMain: true,
        }),
        { version: "0.48.0", is_release_tag: "true", release_sha: RELEASE_SHA },
    );
});

test("automatic mode rejects the caller commit when it differs from the release commit", () => {
    assert.throws(
        () =>
            validateReleaseContext({
                version: "0.48.0",
                mode: "automatic",
                eventName: "push",
                releaseVersion: "0.48.0",
                releaseTag: "v0.48.0",
                releaseSha: RELEASE_SHA,
                checkedOutSha: CALLER_SHA,
                tagSha: RELEASE_SHA,
                commitOnMain: true,
            }),
        /do not agree/,
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
                    mode: "automatic",
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
                    mode: "manual",
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
                mode: "manual",
                releaseVersion: "2.3.4",
                releaseTag: "v2.3.4",
                releaseSha: "1".repeat(40),
                checkedOutSha: "1".repeat(40),
            }),
        /does not accept publication inputs/,
    );
});

test("release mode is explicit", () => {
    assert.throws(
        () =>
            validateReleaseContext({
                version: "2.3.4",
                mode: "push",
                checkedOutSha: RELEASE_SHA,
            }),
        /must be automatic or manual/,
    );
});
