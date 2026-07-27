#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";

import { classifySemanticRelease } from "./semantic-release-result.mjs";

const BEFORE = "1".repeat(40);
const RELEASE = "2".repeat(40);
const TAG_OBJECT = "3".repeat(40);

function validRelease(overrides = {}) {
    return {
        beforeVersion: "1.2.3",
        afterVersion: "1.3.0",
        beforeSha: BEFORE,
        headSha: RELEASE,
        headParentSha: BEFORE,
        tagCommitSha: RELEASE,
        localTagObjectSha: TAG_OBJECT,
        remoteMainSha: RELEASE,
        remoteTagObjectSha: TAG_OBJECT,
        clean: true,
        ...overrides,
    };
}

test("a validated semantic-release commit exposes version, tag, and SHA", () => {
    assert.deepEqual(classifySemanticRelease(validRelease()), {
        released: "true",
        version: "1.3.0",
        tag: "v1.3.0",
        sha: RELEASE,
    });
});

test("no releasable change produces no automatic release request", () => {
    assert.deepEqual(
        classifySemanticRelease({
            beforeVersion: "1.2.3",
            afterVersion: "1.2.3",
            beforeSha: BEFORE,
            headSha: BEFORE,
            clean: true,
        }),
        { released: "false", version: "1.2.3", tag: "", sha: "" },
    );
});

test("divergent version, tag, SHA, parent, remote main, and dirty state are rejected", () => {
    for (const [overrides, pattern] of [
        [{ afterVersion: "1.3.0-beta" }, /stable semantic version/],
        [{ tagCommitSha: BEFORE }, /do not agree/],
        [{ headParentSha: RELEASE }, /direct child/],
        [{ remoteMainSha: BEFORE }, /do not agree/],
        [{ remoteTagObjectSha: BEFORE }, /tags do not agree/],
        [{ clean: false }, /uncommitted changes/],
    ]) {
        assert.throws(() => classifySemanticRelease(validRelease(overrides)), pattern);
    }
});

test("HEAD cannot change when semantic-release reports no version change", () => {
    assert.throws(
        () =>
            classifySemanticRelease({
                beforeVersion: "1.2.3",
                afterVersion: "1.2.3",
                beforeSha: BEFORE,
                headSha: RELEASE,
                clean: true,
            }),
        /changed HEAD/,
    );
});
