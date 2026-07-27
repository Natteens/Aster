#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";
import {
    mkdirSync,
    mkdtempSync,
    readFileSync,
    readdirSync,
    renameSync,
    rmSync,
    symlinkSync,
    writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
    INSTALLER_NAMES,
    prepareReleaseAssets,
    releaseAssetNames,
    validateReleaseAssets,
} from "./prepare-release-assets.mjs";
import {
    checksumText,
    releaseArtifactNames,
    sha256,
} from "./package-release.mjs";

const VERSION = "1.2.3";

function temporaryRepository(label) {
    const root = mkdtempSync(join(tmpdir(), `aster-release-assets-${label}-`));
    mkdirSync(join(root, "dist", "release-inputs"), { recursive: true });
    mkdirSync(join(root, "install"));
    for (const name of INSTALLER_NAMES) {
        writeFileSync(join(root, "install", name), `fixture ${name}\n`);
    }
    return root;
}

function writeInput(root, target, version = VERSION, archive = Buffer.from(`archive:${target}`)) {
    const directory = join(root, "dist", "release-inputs", target);
    mkdirSync(directory);
    const { archiveName } = releaseArtifactNames(version, target);
    const hash = sha256(archive);
    writeFileSync(join(directory, archiveName), archive);
    writeFileSync(join(directory, `${archiveName}.sha256`), checksumText(hash, archiveName));
    writeFileSync(
        join(directory, "release-artifact.json"),
        `${JSON.stringify(
            {
                schema: 1,
                product: "aster",
                version,
                target,
                archive: archiveName,
                sha256: hash,
                size: archive.length,
            },
            null,
            2,
        )}\n`,
    );
    return { directory, archiveName, archive, hash };
}

function completeInputs(root, version = VERSION) {
    return {
        linux: writeInput(root, "linux-x64", version),
        windows: writeInput(root, "windows-x64", version),
    };
}

function snapshot(directory) {
    return Object.fromEntries(
        readdirSync(directory)
            .sort()
            .map((name) => [name, sha256(readFileSync(join(directory, name)))]),
    );
}

function mutateManifest(directory, mutate) {
    const path = join(directory, "release-artifact.json");
    const manifest = JSON.parse(readFileSync(path, "utf8"));
    mutate(manifest);
    writeFileSync(path, `${JSON.stringify(manifest, null, 2)}\n`);
}

test("valid Windows and Linux inputs produce the exact deterministic release assets", () => {
    const root = temporaryRepository("valid path with spaces ü");
    try {
        const inputs = completeInputs(root);
        const manifest = prepareReleaseAssets({ repositoryRoot: root });
        const output = join(root, "dist", "release-assets");
        assert.equal(manifest.version, VERSION);
        assert.deepEqual(readdirSync(output).sort(), releaseAssetNames(VERSION));
        assert.deepEqual(
            manifest.assets.map((asset) => asset.target),
            ["linux-x64", "windows-x64"],
        );
        for (const [target, input] of [
            ["linux-x64", inputs.linux],
            ["windows-x64", inputs.windows],
        ]) {
            const extension = target === "windows-x64" ? ".zip" : ".tar.gz";
            const alias = `aster-${target}${extension}`;
            assert.deepEqual(readFileSync(join(output, alias)), input.archive);
            assert.equal(
                readFileSync(join(output, `${alias}.sha256`), "utf8"),
                checksumText(input.hash, alias),
            );
        }
        const manifestText = readFileSync(join(output, "release-manifest.json"), "utf8");
        assert.ok(manifestText.endsWith("\n"));
        assert.ok(!manifestText.includes("timestamp"));
        assert.ok(!manifestText.includes(root));
        validateReleaseAssets({ repositoryRoot: root });

        const first = snapshot(output);
        prepareReleaseAssets({ repositoryRoot: root });
        assert.deepEqual(snapshot(output), first);
    } finally {
        rmSync(root, { recursive: true, force: true });
    }
});

test("version divergence and target mismatches are rejected", async (context) => {
    await context.test("different versions", () => {
        const root = temporaryRepository("versions");
        try {
            writeInput(root, "linux-x64", "1.2.3");
            writeInput(root, "windows-x64", "1.2.4");
            assert.throws(
                () => prepareReleaseAssets({ repositoryRoot: root }),
                /versions do not match/,
            );
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    });
    await context.test("reported target differs from directory", () => {
        const root = temporaryRepository("target");
        try {
            const inputs = completeInputs(root);
            mutateManifest(inputs.windows.directory, (manifest) => {
                manifest.target = "linux-x64";
            });
            assert.throws(
                () => prepareReleaseAssets({ repositoryRoot: root }),
                /reports target linux-x64/,
            );
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    });
});

test("missing targets and unexpected input files are rejected", async (context) => {
    await context.test("missing target", () => {
        const root = temporaryRepository("missing");
        try {
            writeInput(root, "windows-x64");
            assert.throws(
                () => prepareReleaseAssets({ repositoryRoot: root }),
                /exactly linux-x64 and windows-x64/,
            );
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    });
    await context.test("unexpected file", () => {
        const root = temporaryRepository("unexpected");
        try {
            const inputs = completeInputs(root);
            writeFileSync(join(inputs.linux.directory, "debug.log"), "unexpected");
            assert.throws(
                () => prepareReleaseAssets({ repositoryRoot: root }),
                /unexpected file: debug\.log/,
            );
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    });
});

test("checksum, size, and archive tampering are rejected", async (context) => {
    for (const [label, mutate, pattern] of [
        [
            "checksum",
            (input) =>
                mutateManifest(input.directory, (manifest) => {
                    manifest.sha256 = "0".repeat(64);
                }),
            /SHA-256 does not match/,
        ],
        [
            "size",
            (input) =>
                mutateManifest(input.directory, (manifest) => {
                    manifest.size += 1;
                }),
            /size does not match/,
        ],
        [
            "archive",
            (input) => writeFileSync(join(input.directory, input.archiveName), "tampered"),
            /size does not match|SHA-256 does not match/,
        ],
    ]) {
        await context.test(label, () => {
            const root = temporaryRepository(label);
            try {
                const inputs = completeInputs(root);
                mutate(inputs.windows);
                assert.throws(
                    () => prepareReleaseAssets({ repositoryRoot: root }),
                    pattern,
                );
            } finally {
                rmSync(root, { recursive: true, force: true });
            }
        });
    }
});

test("missing installers and unexpected existing output are preserved and rejected", async (context) => {
    await context.test("missing installer", () => {
        const root = temporaryRepository("installer");
        try {
            completeInputs(root);
            rmSync(join(root, "install", "uninstall.sh"));
            assert.throws(
                () => prepareReleaseAssets({ repositoryRoot: root }),
                /installer uninstall\.sh is missing/,
            );
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    });
    await context.test("unexpected output", () => {
        const root = temporaryRepository("existing");
        try {
            completeInputs(root);
            const output = join(root, "dist", "release-assets");
            mkdirSync(output);
            writeFileSync(join(output, "owned.txt"), "owned");
            assert.throws(
                () => prepareReleaseAssets({ repositoryRoot: root }),
                /unexpected file: owned\.txt/,
            );
            assert.equal(readFileSync(join(output, "owned.txt"), "utf8"), "owned");
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    });
});

test("output outside dist/release-assets is rejected before writing", () => {
    const root = temporaryRepository("outside");
    try {
        completeInputs(root);
        const outside = join(root, "elsewhere");
        assert.throws(
            () =>
                prepareReleaseAssets({
                    repositoryRoot: root,
                    outputDirectory: outside,
                }),
            /must be dist\/release-assets/,
        );
        assert.equal(readdirSync(root).includes("elsewhere"), false);
    } finally {
        rmSync(root, { recursive: true, force: true });
    }
});

test("a symlinked dist ancestor cannot redirect release output", (context) => {
    const root = temporaryRepository("symlink");
    const external = `${root}-external`;
    try {
        completeInputs(root);
        renameSync(join(root, "dist"), external);
        try {
            symlinkSync(external, join(root, "dist"), "junction");
        } catch (error) {
            if (error.code === "EPERM" || error.code === "EACCES") {
                context.skip("host cannot create a test junction");
                return;
            }
            throw error;
        }
        assert.throws(
            () => prepareReleaseAssets({ repositoryRoot: root }),
            /symlinked path component/,
        );
        assert.equal(readdirSync(external).includes("release-assets"), false);
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(external, { recursive: true, force: true });
    }
});

test("validation rejects a changed alias and a changed aggregate manifest", async (context) => {
    await context.test("alias", () => {
        const root = temporaryRepository("alias");
        try {
            completeInputs(root);
            prepareReleaseAssets({ repositoryRoot: root });
            writeFileSync(join(root, "dist", "release-assets", "aster-windows-x64.zip"), "bad");
            assert.throws(
                () => validateReleaseAssets({ repositoryRoot: root }),
                /alias is not byte-for-byte identical/,
            );
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    });
    await context.test("manifest order", () => {
        const root = temporaryRepository("manifest");
        try {
            completeInputs(root);
            prepareReleaseAssets({ repositoryRoot: root });
            const path = join(root, "dist", "release-assets", "release-manifest.json");
            const manifest = JSON.parse(readFileSync(path, "utf8"));
            manifest.assets.reverse();
            writeFileSync(path, `${JSON.stringify(manifest, null, 2)}\n`);
            assert.throws(
                () => validateReleaseAssets({ repositoryRoot: root }),
                /targets must be unique and sorted/,
            );
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    });
    await context.test("manifest filenames", () => {
        const root = temporaryRepository("manifest-name");
        try {
            completeInputs(root);
            prepareReleaseAssets({ repositoryRoot: root });
            const path = join(root, "dist", "release-assets", "release-manifest.json");
            const manifest = JSON.parse(readFileSync(path, "utf8"));
            manifest.assets[0].archive = manifest.assets[1].archive;
            writeFileSync(path, `${JSON.stringify(manifest, null, 2)}\n`);
            assert.throws(
                () => validateReleaseAssets({ repositoryRoot: root }),
                /unexpected filenames/,
            );
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    });
});
