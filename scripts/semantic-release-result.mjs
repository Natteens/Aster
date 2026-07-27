#!/usr/bin/env node

import { appendFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { readVersion } from "./bundle.mjs";

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const SHA = /^[0-9a-f]{40}$/;

function requireVersion(value, label) {
    if (!SEMVER.test(value)) throw new Error(`${label} is not a stable semantic version`);
}

function requireSha(value, label) {
    if (!SHA.test(value)) throw new Error(`${label} is not a full commit identifier`);
}

export function classifySemanticRelease({
    beforeVersion,
    afterVersion,
    beforeSha,
    headSha,
    headParentSha,
    tagCommitSha,
    localTagObjectSha,
    remoteMainSha,
    remoteTagObjectSha,
    clean,
}) {
    requireVersion(beforeVersion, "Previous version");
    requireVersion(afterVersion, "Current version");
    requireSha(beforeSha, "Trigger SHA");
    requireSha(headSha, "Release SHA");
    if (!clean) throw new Error("semantic-release left uncommitted changes");

    if (afterVersion === beforeVersion) {
        if (headSha !== beforeSha) {
            throw new Error("semantic-release changed HEAD without changing the version");
        }
        return { released: "false", version: afterVersion, tag: "", sha: "" };
    }

    for (const [value, label] of [
        [headParentSha, "Release parent SHA"],
        [tagCommitSha, "Tag commit SHA"],
        [localTagObjectSha, "Local tag object SHA"],
        [remoteMainSha, "Remote main SHA"],
        [remoteTagObjectSha, "Remote tag object SHA"],
    ]) {
        requireSha(value, label);
    }
    if (headParentSha !== beforeSha) {
        throw new Error("release commit is not a direct child of the validated main commit");
    }
    if (tagCommitSha !== headSha || remoteMainSha !== headSha) {
        throw new Error("release tag, release commit, and remote main do not agree");
    }
    if (remoteTagObjectSha !== localTagObjectSha) {
        throw new Error("local and remote release tags do not agree");
    }
    return {
        released: "true",
        version: afterVersion,
        tag: `v${afterVersion}`,
        sha: headSha,
    };
}

function git(repositoryRoot, arguments_, { allowEmpty = false } = {}) {
    const result = spawnSync("git", arguments_, {
        cwd: repositoryRoot,
        encoding: "utf8",
    });
    if (result.error) throw new Error(`git ${arguments_[0]} failed: ${result.error.message}`);
    if (result.status !== 0) {
        throw new Error(`git ${arguments_[0]} failed`);
    }
    const output = result.stdout.trim();
    if (!allowEmpty && output.length === 0) {
        throw new Error(`git ${arguments_[0]} returned no result`);
    }
    return output;
}

function remoteRef(repositoryRoot, ref) {
    const output = git(repositoryRoot, ["ls-remote", "--refs", "origin", ref]);
    const lines = output.split(/\r?\n/).filter(Boolean);
    if (lines.length !== 1) throw new Error(`remote ref ${ref} is missing or ambiguous`);
    const [sha, name] = lines[0].split(/\s+/);
    if (name !== ref) throw new Error(`remote ref ${ref} returned an unexpected name`);
    return sha;
}

export function inspectSemanticRelease(repositoryRoot, beforeVersion, beforeSha) {
    const afterVersion = readVersion(join(repositoryRoot, "Cargo.toml"));
    const headSha = git(repositoryRoot, ["rev-parse", "HEAD"]);
    const clean = git(repositoryRoot, ["status", "--porcelain"], { allowEmpty: true }) === "";
    if (afterVersion === beforeVersion) {
        return classifySemanticRelease({
            beforeVersion,
            afterVersion,
            beforeSha,
            headSha,
            clean,
        });
    }

    const tag = `v${afterVersion}`;
    return classifySemanticRelease({
        beforeVersion,
        afterVersion,
        beforeSha,
        headSha,
        headParentSha: git(repositoryRoot, ["rev-parse", "HEAD^"]),
        tagCommitSha: git(repositoryRoot, ["rev-parse", `${tag}^{commit}`]),
        localTagObjectSha: git(repositoryRoot, ["rev-parse", `refs/tags/${tag}`]),
        remoteMainSha: remoteRef(repositoryRoot, "refs/heads/main"),
        remoteTagObjectSha: remoteRef(repositoryRoot, `refs/tags/${tag}`),
        clean,
    });
}

const isMain = process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
    try {
        if (process.argv.length !== 4) {
            throw new Error("usage: semantic-release-result PREVIOUS_VERSION TRIGGER_SHA");
        }
        const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
        const result = inspectSemanticRelease(repositoryRoot, process.argv[2], process.argv[3]);
        if (!process.env.GITHUB_OUTPUT) {
            throw new Error("GITHUB_OUTPUT is required");
        }
        appendFileSync(
            process.env.GITHUB_OUTPUT,
            [
                `released=${result.released}`,
                `version=${result.version}`,
                `release_tag=${result.tag}`,
                `release_sha=${result.sha}`,
                "",
            ].join("\n"),
        );
        console.log(
            result.released === "true"
                ? `semantic-release created ${result.tag} at ${result.sha}`
                : "semantic-release found no releasable changes",
        );
    } catch (error) {
        console.error(`error: ${error.message}`);
        process.exit(1);
    }
}
