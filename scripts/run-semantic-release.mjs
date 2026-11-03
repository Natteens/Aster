#!/usr/bin/env node

// Runs semantic-release for CI and tolerates exactly one failure: the optional
// Git Notes push that semantic-release performs AFTER it has already pushed the
// release commit and the version tag.
//
// semantic-release 25 records the release channel of every tag in a per-tag
// notes ref (`refs/notes/semantic-release-v<version>`) and pushes it last
// (lib/git.js `pushNotes`, called from index.js after `push`). When a tag has no
// note, `getTagsNotes` consumers fall back to `{ channels: [null] }` (the
// default channel), which is exactly what a `main` release would have recorded.
// So losing that one ref cannot change later version calculation, while failing
// the job here would block the validated release pipeline that follows.
//
// Every other failure is propagated unchanged, and the tolerance applies only
// when the release itself is verified with the same checks the workflow runs
// afterwards (`inspectSemanticRelease`).

import { spawn, spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { readVersion } from "./bundle.mjs";
import { inspectSemanticRelease } from "./semantic-release-result.mjs";

const ERROR_MARKER = "An error occurred while running semantic-release";

function escapeForRegExp(value) {
    return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Decide what a finished semantic-release run means.
 *
 * Returns `{ outcome: "success" }` for exit code 0 and
 * `{ outcome: "notes-only", notesRef }` when the only failure was the rejected
 * push of this release's notes ref after a verified release. Anything else
 * throws.
 */
export function classifyReleaseRun({ exitCode, output, beforeVersion, afterVersion, integrityError }) {
    if (exitCode === 0) return { outcome: "success" };

    const failure = (reason) => new Error(`semantic-release failed: ${reason}`);
    if (afterVersion === beforeVersion) throw failure("no release was created");

    const notesRef = `refs/notes/semantic-release-v${afterVersion}`;
    const escapedRef = escapeForRegExp(notesRef);
    const errorCount = output.split(ERROR_MARKER).length - 1;
    const notesPush = new RegExp(
        `${ERROR_MARKER}: ExecaError: Command failed with exit code 1: git push -- '[^']+' ${escapedRef}(?:\\s|$)`,
    );
    const rejection = ` ! [remote rejected] ${notesRef} -> ${notesRef} (failed)`;
    if (errorCount !== 1 || !notesPush.test(output) || !output.includes(rejection)) {
        throw failure("the failure is not the optional Git Notes push of this release");
    }
    if (integrityError) throw failure(integrityError);
    return { outcome: "notes-only", notesRef };
}

function runSemanticRelease(repositoryRoot) {
    return new Promise((resolve, reject) => {
        const binary = join(repositoryRoot, "node_modules", "semantic-release", "bin", "semantic-release.js");
        const child = spawn(process.execPath, [binary], {
            cwd: repositoryRoot,
            stdio: ["inherit", "pipe", "pipe"],
        });
        let output = "";
        child.stdout.on("data", (chunk) => {
            output += chunk;
            process.stdout.write(chunk);
        });
        child.stderr.on("data", (chunk) => {
            output += chunk;
            process.stderr.write(chunk);
        });
        child.on("error", reject);
        child.on("close", (code) => resolve({ exitCode: code ?? 1, output }));
    });
}

const isMain = process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
    try {
        if (process.argv.length !== 4) {
            throw new Error("usage: run-semantic-release PREVIOUS_VERSION TRIGGER_SHA");
        }
        const [beforeVersion, beforeSha] = process.argv.slice(2);
        const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
        const { exitCode, output } = await runSemanticRelease(repositoryRoot);

        const afterVersion = readVersion(join(repositoryRoot, "Cargo.toml"));
        let integrityError = null;
        if (exitCode !== 0 && afterVersion !== beforeVersion) {
            try {
                inspectSemanticRelease(repositoryRoot, beforeVersion, beforeSha);
            } catch (error) {
                integrityError = error.message;
            }
        }
        const decision = classifyReleaseRun({ exitCode, output, beforeVersion, afterVersion, integrityError });
        if (decision.outcome === "notes-only") {
            // One retry; the ref only records the default channel, so a second failure is not fatal.
            const retry = spawnSync("git", ["push", "--", "origin", decision.notesRef], {
                cwd: repositoryRoot,
                stdio: "inherit",
            });
            console.log(
                `::warning::The release ${afterVersion} was created and verified, but pushing ${decision.notesRef} failed` +
                    (retry.status === 0 ? " once and succeeded on retry." : " twice; later releases treat a missing note as the default channel."),
            );
        }
    } catch (error) {
        console.error(`error: ${error.message}`);
        process.exit(1);
    }
}
