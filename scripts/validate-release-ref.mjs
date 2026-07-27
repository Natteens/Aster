#!/usr/bin/env node

import { appendFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { readVersion } from "./bundle.mjs";

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

export function validateReleaseContext({
    version,
    eventName,
    ref,
    refName,
    commitOnMain = true,
}) {
    if (!SEMVER.test(version)) {
        throw new Error("Canonical version must be MAJOR.MINOR.PATCH without prerelease data");
    }
    if (eventName === "workflow_dispatch") {
        return { version, is_release_tag: "false" };
    }
    if (eventName !== "push" || !ref.startsWith("refs/tags/")) {
        throw new Error("Release workflow accepts only workflow_dispatch or a pushed tag");
    }
    const tag = ref.slice("refs/tags/".length);
    if (!/^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(tag)) {
        throw new Error(`Release tag is not valid: ${tag}`);
    }
    if (tag !== `v${version}` || refName !== tag) {
        throw new Error(`Release tag ${tag} does not match canonical version ${version}`);
    }
    if (!commitOnMain) {
        throw new Error("Release tag commit is not in the origin/main history");
    }
    return { version, is_release_tag: "true" };
}

function taggedCommitIsOnMain(commit) {
    if (!/^[0-9a-f]{40}$/i.test(commit)) {
        throw new Error("GITHUB_SHA is not a full commit identifier");
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

export function validateCurrentReleaseEnvironment(repositoryRoot, environment = process.env) {
    const version = readVersion(join(repositoryRoot, "Cargo.toml"));
    const eventName = environment.GITHUB_EVENT_NAME ?? "workflow_dispatch";
    const isTagPush = eventName === "push";
    return validateReleaseContext({
        version,
        eventName,
        ref: environment.GITHUB_REF ?? "",
        refName: environment.GITHUB_REF_NAME ?? "",
        commitOnMain: isTagPush
            ? taggedCommitIsOnMain(environment.GITHUB_SHA ?? "")
            : true,
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
                `version=${result.version}\nis_release_tag=${result.is_release_tag}\n`,
            );
        }
        console.log(`Release context valid: version=${result.version}, tag=${result.is_release_tag}`);
    } catch (error) {
        console.error(`error: ${error.message}`);
        process.exit(1);
    }
}
