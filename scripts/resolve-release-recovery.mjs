#!/usr/bin/env node

// Verifies that a version tag is an interrupted release (release commit and tag
// exist on main, public GitHub Release is missing) and resolves the exact
// version, tag and commit the existing validated release pipeline must publish.
// It only reads Git and GitHub state; it never creates versions, tags or releases.

import { appendFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { parseWorkspaceVersion } from "./bundle.mjs";
import { git, remoteRef } from "./git-state.mjs";

const TAG = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const SHA = /^[0-9a-f]{40}$/;

function parseTag(tag) {
    const match = TAG.exec(tag);
    return match ? match.slice(1).map(Number) : null;
}

/** The newest `vMAJOR.MINOR.PATCH` tag by numeric version; empty when none. */
export function highestReleaseTag(tags) {
    let best = "";
    let bestParts = null;
    for (const tag of tags) {
        const parts = parseTag(tag);
        if (!parts) continue;
        const newer =
            !bestParts ||
            parts[0] > bestParts[0] ||
            (parts[0] === bestParts[0] &&
                (parts[1] > bestParts[1] || (parts[1] === bestParts[1] && parts[2] > bestParts[2])));
        if (newer) {
            best = tag;
            bestParts = parts;
        }
    }
    return best;
}

/**
 * Pure decision over already-gathered facts. Throws unless the tag is the
 * newest, canonical, unpublished release on main.
 */
export function classifyReleaseRecovery({
    tag,
    dispatchRef,
    headSha,
    remoteMainSha,
    tagCommitSha,
    localTagObjectSha,
    remoteTagObjectSha,
    versionAtTag,
    commitSubject,
    tagOnMain,
    highestTag,
    releaseExists,
}) {
    if (!parseTag(tag)) throw new Error(`${tag} is not a release tag`);
    if (dispatchRef !== "refs/heads/main") {
        throw new Error("recovery must be dispatched from the main branch");
    }
    for (const [value, label] of [
        [headSha, "Checked out SHA"],
        [remoteMainSha, "Remote main SHA"],
        [tagCommitSha, "Tag commit SHA"],
        [localTagObjectSha, "Local tag object SHA"],
        [remoteTagObjectSha, "Remote tag object SHA"],
    ]) {
        if (!SHA.test(value ?? "")) throw new Error(`${label} is not a full commit identifier`);
    }
    if (headSha !== remoteMainSha) {
        throw new Error("the checkout is not the current origin/main");
    }
    if (localTagObjectSha !== remoteTagObjectSha) {
        throw new Error("local and remote release tags do not agree");
    }
    const version = tag.slice(1);
    if (versionAtTag !== version) {
        throw new Error(`Cargo.toml at ${tag} is ${versionAtTag}, which does not match the tag`);
    }
    if (commitSubject !== `chore(release): ${version} [skip ci]`) {
        throw new Error(`${tag} does not point at the canonical release commit`);
    }
    if (!tagOnMain) throw new Error(`${tag} is not in the origin/main history`);
    if (highestTag !== tag) {
        throw new Error(`${tag} is not the newest release tag (${highestTag || "none"})`);
    }
    if (releaseExists === true) throw new Error(`release ${tag} is already published`);
    if (releaseExists !== false) throw new Error(`the state of release ${tag} could not be determined`);
    return { version, tag, sha: tagCommitSha };
}

function releasePublished(repositoryRoot, tag) {
    const result = spawnSync("gh", ["release", "view", tag], { cwd: repositoryRoot, encoding: "utf8" });
    if (result.error) return undefined;
    if (result.status === 0) return true;
    return /release not found/i.test(result.stderr ?? "") ? false : undefined;
}

export function inspectReleaseRecovery(repositoryRoot, tag, environment = process.env) {
    if (!parseTag(tag)) throw new Error(`${tag} is not a release tag`);
    const tagCommitSha = git(repositoryRoot, ["rev-parse", `refs/tags/${tag}^{commit}`]);
    const ancestor = spawnSync(
        "git",
        ["merge-base", "--is-ancestor", tagCommitSha, "refs/remotes/origin/main"],
        { cwd: repositoryRoot },
    );
    if (ancestor.error || (ancestor.status !== 0 && ancestor.status !== 1)) {
        throw new Error("could not inspect origin/main history");
    }
    return classifyReleaseRecovery({
        tag,
        dispatchRef: environment.GITHUB_REF ?? "",
        headSha: git(repositoryRoot, ["rev-parse", "HEAD"]),
        remoteMainSha: remoteRef(repositoryRoot, "refs/heads/main"),
        tagCommitSha,
        localTagObjectSha: git(repositoryRoot, ["rev-parse", `refs/tags/${tag}`]),
        remoteTagObjectSha: remoteRef(repositoryRoot, `refs/tags/${tag}`),
        versionAtTag: parseWorkspaceVersion(git(repositoryRoot, ["show", `${tagCommitSha}:Cargo.toml`]), `${tag}:Cargo.toml`),
        commitSubject: git(repositoryRoot, ["log", "-1", "--format=%s", tagCommitSha]),
        tagOnMain: ancestor.status === 0,
        highestTag: highestReleaseTag(git(repositoryRoot, ["tag", "--list", "v*"]).split(/\r?\n/)),
        releaseExists: releasePublished(repositoryRoot, tag),
    });
}

const isMain = process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
    try {
        if (process.argv.length !== 2) throw new Error("resolve-release-recovery does not accept arguments");
        const tag = process.env.ASTER_RECOVERY_TAG ?? "";
        const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
        const result = inspectReleaseRecovery(repositoryRoot, tag);
        if (!process.env.GITHUB_OUTPUT) throw new Error("GITHUB_OUTPUT is required");
        appendFileSync(
            process.env.GITHUB_OUTPUT,
            [`version=${result.version}`, `release_tag=${result.tag}`, `release_sha=${result.sha}`, ""].join("\n"),
        );
        console.log(`Recoverable release ${result.tag} at ${result.sha}`);
    } catch (error) {
        console.error(`error: ${error.message}`);
        process.exit(1);
    }
}
