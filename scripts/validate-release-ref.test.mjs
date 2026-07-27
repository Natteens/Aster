#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";

import { validateReleaseContext } from "./validate-release-ref.mjs";

test("manual release validation uses the canonical version without publishing", () => {
    assert.deepEqual(
        validateReleaseContext({
            version: "2.3.4",
            eventName: "workflow_dispatch",
            ref: "refs/heads/main",
            refName: "main",
        }),
        { version: "2.3.4", is_release_tag: "false" },
    );
});

test("a matching release tag on main is publishable", () => {
    assert.deepEqual(
        validateReleaseContext({
            version: "2.3.4",
            eventName: "push",
            ref: "refs/tags/v2.3.4",
            refName: "v2.3.4",
            commitOnMain: true,
        }),
        { version: "2.3.4", is_release_tag: "true" },
    );
});

test("invalid tags, divergent versions, and off-main commits are rejected", () => {
    for (const [overrides, pattern] of [
        [{ ref: "refs/tags/v2.3" }, /not valid/],
        [{ ref: "refs/tags/v2.3.4-beta" }, /not valid/],
        [{ ref: "refs/tags/release-2.3.4" }, /not valid/],
        [{ ref: "refs/tags/v02.3.4" }, /not valid/],
        [{ ref: "refs/tags/v2.3.5", refName: "v2.3.5" }, /does not match/],
        [{ commitOnMain: false }, /not in the origin\/main history/],
    ]) {
        assert.throws(
            () =>
                validateReleaseContext({
                    version: "2.3.4",
                    eventName: "push",
                    ref: "refs/tags/v2.3.4",
                    refName: "v2.3.4",
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
                    ref: "refs/heads/main",
                    refName: "main",
                }),
            /Canonical version/,
        );
    }
});
