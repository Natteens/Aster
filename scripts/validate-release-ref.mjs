#!/usr/bin/env node

import { appendFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { readVersion } from "./bundle.mjs";

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

export function validateReleaseContext({
    version,
    mode,
    releaseVersion = "",
    releaseTag = "",
    releaseSha = "",
    checkedOutSha = "",
    tagSha = "",
    commitOnMain = true,
}) {
    if (!SEMVER.test(version)) {
        throw new Error("Canonical version must be MAJOR.MINOR.PATCH without prerelease data");
    }
    if (mode === "manual") {
        if (releaseVersion || releaseTag || releaseSha) {
            throw new Error("Manual release validation does not accept publication inputs");
        }
        return { version, is_release_tag: "false", release_sha: checkedOutSha };
    }
    if (mode !== "automatic") {
        throw new Error("Release mode must be automatic or manual");
    }
    if (!/^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(releaseTag)) {
        throw new Error(`Release tag is not valid: ${releaseTag}`);
    }
    if (releaseVersion !== version || releaseTag !== `v${version}`) {
        throw new Error(`Release tag ${releaseTag} does not match canonical version ${version}`);
    }
    if (
        !/^[0-9a-f]{40}$/.test(releaseSha) ||
        releaseSha !== checkedOutSha ||
        releaseSha !== tagSha
    ) {
        throw new Error("Release SHA, checked out commit, and tag do not agree");
    }
    if (!commitOnMain) {
        throw new Error("Release tag commit is not in the origin/main history");
    }
    return { version, is_release_tag: "true", release_sha: releaseSha };
}

function taggedCommitIsOnMain(commit) {
    if (!/^[0-9a-f]{40}$/i.test(commit)) {
        throw new Error("Release SHA is not a full commit identifier");
    }
    const result = spawnSync(
        "git",
        ["merge-base", "--is-ancestor", commit, "refs/remotes/origin/main"],
        { stdio: "ignore" },
    );
    if (result.error) throw new Error(`Could not validate main history: ${result.error.message}`);
    if (result.status === 0) return true;
    if (result.status === 1) return false;
    throw new Error("Could not inspect origin/main history");
}

function resolveGitCommit(repositoryRoot, revision) {
    const result = spawnSync("git", ["rev-parse", revision], {
        cwd: repositoryRoot,
        encoding: "utf8",
    });
    if (result.error || result.status !== 0) {
        throw new Error(`Could not resolve ${revision}`);
    }
    const commit = result.stdout.trim();
    if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error(`${revision} is not a full commit`);
    return commit;
}

export function validateCurrentReleaseEnvironment(repositoryRoot, environment = process.env) {
    const version = readVersion(join(repositoryRoot, "Cargo.toml"));
    const mode = environment.ASTER_RELEASE_MODE ?? "";
    const releaseSha = environment.ASTER_RELEASE_SHA ?? "";
    const releaseTag = environment.ASTER_RELEASE_TAG ?? "";
    const checkedOutSha = resolveGitCommit(repositoryRoot, "HEAD");
    return validateReleaseContext({
        version,
        mode,
        releaseVersion: environment.ASTER_RELEASE_VERSION ?? "",
        releaseTag,
        releaseSha,
        checkedOutSha,
        tagSha:
            mode === "automatic" ? resolveGitCommit(repositoryRoot, `${releaseTag}^{commit}`) : "",
        commitOnMain: mode === "automatic" ? taggedCommitIsOnMain(releaseSha) : true,
    });
}

const isMain = process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
    try {
        if (process.argv.length !== 2) {
            throw new Error("validate-release-ref does not accept arguments");
        }
        const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
        const result = validateCurrentReleaseEnvironment(repositoryRoot);
        if (process.env.GITHUB_OUTPUT) {
            appendFileSync(
                process.env.GITHUB_OUTPUT,
                [
                    `version=${result.version}`,
                    `is_release_tag=${result.is_release_tag}`,
                    `release_sha=${result.release_sha}`,
                    "",
                ].join("\n"),
            );
        }
        console.log(`Release context valid: version=${result.version}, tag=${result.is_release_tag}`);
    } catch (error) {
        console.error(`error: ${error.message}`);
        process.exit(1);
    }
}
