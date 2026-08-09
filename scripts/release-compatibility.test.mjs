import assert from "node:assert/strict";
import test from "node:test";

import { analyzeCommits } from "@semantic-release/commit-analyzer";

import releaseConfig from "../release.config.mjs";

function analyzerConfig() {
    const entry = releaseConfig.plugins.find(
        (plugin) => Array.isArray(plugin) && plugin[0] === "@semantic-release/commit-analyzer",
    );
    assert.ok(entry, "release config must define the commit analyzer");
    return entry[1];
}

async function analyze(message) {
    return analyzeCommits(analyzerConfig(), {
        commits: [{ hash: "0123456789abcdef", message }],
        cwd: process.cwd(),
        logger: { log() {} },
    });
}

test("pre-1.0 breaking commits produce a minor release", async () => {
    assert.equal(
        await analyze(
            "feat(language)!: change a public contract\n\nBREAKING CHANGE: migrate to the new form",
        ),
        "minor",
    );
});

test("ordinary release rules still fall back to semantic-release defaults", async () => {
    assert.equal(await analyze("feat(language): add behavior"), "minor");
    assert.equal(await analyze("fix(cli): preserve an exit code"), "patch");
    assert.equal(await analyze("docs: clarify a reference page"), null);
});
