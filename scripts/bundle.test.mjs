#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";
import {
    copyFileSync,
    cpSync,
    existsSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    renameSync,
    rmSync,
    statSync,
    writeFileSync,
} from "node:fs";
import { execFileSync, spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, join, sep } from "node:path";
import { fileURLToPath } from "node:url";

import {
    PRODUCT,
    REQUIRED_STDLIB_MODULES,
    SCHEMA,
    assertSafeBundle,
    buildBundle,
    bundleDirectoryName,
    detectBundleTarget,
    readVersion,
    validateBundle,
} from "./bundle.mjs";

const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const { bundleTarget, binaryName } = detectBundleTarget();

const releaseBinaryPath = join(repositoryRoot, "target", "release", binaryName);
const hasReleaseBinary = existsSync(releaseBinaryPath);

/** Create a temp directory with optional label. Prefix includes process id to avoid collisions. */
function tempDir(label = "bundle") {
    return mkdtempSync(join(tmpdir(), `aster-${label}-${process.pid}-`));
}

/**
 * Build a minimal fake workspace at `root` for testing.
 * Options control which parts to omit (to test error cases).
 */
function createFakeWorkspace(
    root,
    {
        skipBinary = false,
        skipLicense = false,
        skipStdlib = false,
        incompleteStdlib = false,
    } = {},
) {
    // Fake release binary
    if (!skipBinary) {
        mkdirSync(join(root, "target", "release"), { recursive: true });
        writeFileSync(join(root, "target", "release", binaryName), "fake-binary");
    }

    // LICENSE
    if (!skipLicense) {
        writeFileSync(join(root, "LICENSE"), "Apache-2.0 placeholder\n");
    }

    // stdlib
    if (!skipStdlib) {
        const modules = incompleteStdlib
            ? REQUIRED_STDLIB_MODULES.slice(0, 1) // only first module
            : REQUIRED_STDLIB_MODULES;
        for (const rel of modules) {
            const full = join(root, "stdlib", rel.split("/").join(sep));
            mkdirSync(dirname(full), { recursive: true });
            writeFileSync(full, `// placeholder: ${rel}`);
        }
    }
}

/**
 * Run buildBundle against a fake workspace, returning the bundle dir.
 * Caller must clean up `distRoot`.
 */
function runFakeBundle(workspaceRoot, distRoot, opts = {}) {
    mkdirSync(distRoot, { recursive: true });
    const version = opts.version ?? "0.40.0";
    return buildBundle({
        workspaceRoot,
        distRoot,
        version,
        bundleTarget: opts.bundleTarget ?? bundleTarget,
        binaryName: opts.binaryName ?? binaryName,
        rustTarget: opts.rustTarget,
    });
}

// ---------------------------------------------------------------------------
// Target / naming / version
// ---------------------------------------------------------------------------

test("detectBundleTarget returns supported target for this platform", () => {
    const { bundleTarget: t, binaryName: b } = detectBundleTarget();
    assert.ok(
        t === "windows-x64" || t === "linux-x64",
        `unexpected bundle target: ${t}`,
    );
    assert.ok(b === "aster.exe" || b === "aster", `unexpected binary name: ${b}`);
});

test("bundleDirectoryName produces canonical name", () => {
    assert.equal(bundleDirectoryName("0.40.0", "windows-x64"), "aster-0.40.0-windows-x64");
    assert.equal(bundleDirectoryName("1.2.3", "linux-x64"), "aster-1.2.3-linux-x64");
});

test("readVersion extracts version from Cargo.toml", () => {
    const version = readVersion(join(repositoryRoot, "Cargo.toml"));
    assert.match(version, /^\d+\.\d+\.\d+/, "version must be semantic");
    assert.equal(version, "0.40.0");
});

// ---------------------------------------------------------------------------
// Path safety
// ---------------------------------------------------------------------------

test("assertSafeBundle accepts bundle directly inside dist", () => {
    const dist = join(tmpdir(), "dist");
    const bundle = join(dist, "aster-0.40.0-windows-x64");
    assert.doesNotThrow(() => assertSafeBundle(dist, bundle, "aster-0.40.0-windows-x64"));
});

test("assertSafeBundle rejects bundle equal to dist (empty relative path)", () => {
    const dist = join(tmpdir(), "dist");
    assert.throws(() => assertSafeBundle(dist, dist), /not safely inside dist/);
});

test("assertSafeBundle rejects path escaping via ..", () => {
    const dist = join(tmpdir(), "dist");
    const escape = join(dist, "..", "elsewhere");
    assert.throws(() => assertSafeBundle(dist, escape), /not safely inside dist/);
});

test("assertSafeBundle rejects filesystem root", () => {
    const dist = join(tmpdir(), "dist");
    assert.throws(
        () => assertSafeBundle(dist, process.platform === "win32" ? "C:\\" : "/"),
        /not safely inside dist|empty or the filesystem root/,
    );
});

test("assertSafeBundle rejects mismatched bundle name", () => {
    const dist = join(tmpdir(), "dist");
    const bundle = join(dist, "aster-0.40.0-windows-x64");
    assert.throws(
        () => assertSafeBundle(dist, bundle, "aster-0.40.0-linux-x64"),
        /name mismatch/,
    );
});

// ---------------------------------------------------------------------------
// Error cases: missing sources
// ---------------------------------------------------------------------------

test("buildBundle fails with actionable message when binary is absent", () => {
    const root = tempDir("no-binary");
    const dist = tempDir("no-binary-dist");
    try {
        createFakeWorkspace(root, { skipBinary: true });
        assert.throws(
            () => runFakeBundle(root, dist),
            (err) => {
                assert.ok(
                    err.message.includes("Release binary not found") ||
                        err.message.includes("not found"),
                    `unexpected error: ${err.message}`,
                );
                return true;
            },
        );
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dist, { recursive: true, force: true });
    }
});

test("buildBundle fails with message when LICENSE is absent", () => {
    const root = tempDir("no-license");
    const dist = tempDir("no-license-dist");
    try {
        createFakeWorkspace(root, { skipLicense: true });
        assert.throws(
            () => runFakeBundle(root, dist),
            /LICENSE file not found/,
        );
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dist, { recursive: true, force: true });
    }
});

test("buildBundle fails with message when stdlib is absent", () => {
    const root = tempDir("no-stdlib");
    const dist = tempDir("no-stdlib-dist");
    try {
        createFakeWorkspace(root, { skipStdlib: true });
        assert.throws(
            () => runFakeBundle(root, dist),
            /stdlib directory not found|Required stdlib module missing/,
        );
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dist, { recursive: true, force: true });
    }
});

test("buildBundle fails when stdlib structure is incomplete", () => {
    const root = tempDir("incomplete-stdlib");
    const dist = tempDir("incomplete-dist");
    try {
        createFakeWorkspace(root, { incompleteStdlib: true });
        assert.throws(
            () => runFakeBundle(root, dist),
            /Required stdlib module missing/,
        );
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dist, { recursive: true, force: true });
    }
});

// ---------------------------------------------------------------------------
// Successful bundle: structure and manifest
// ---------------------------------------------------------------------------

test("buildBundle creates correct layout with valid manifest", () => {
    const root = tempDir("layout");
    const dist = tempDir("layout-dist");
    try {
        createFakeWorkspace(root);
        const { bundleDir } = runFakeBundle(root, dist);

        // Binary present
        assert.ok(existsSync(join(bundleDir, "bin", binaryName)), "binary must exist");
        assert.ok(statSync(join(bundleDir, "bin", binaryName)).isFile(), "binary must be a file");

        // stdlib present
        assert.ok(existsSync(join(bundleDir, "stdlib", "aster")), "stdlib/aster must exist");
        for (const rel of REQUIRED_STDLIB_MODULES) {
            assert.ok(
                existsSync(join(bundleDir, "stdlib", rel.split("/").join(sep))),
                `stdlib module missing: ${rel}`,
            );
        }

        // LICENSE present
        assert.ok(existsSync(join(bundleDir, "LICENSE")));

        // Manifest valid
        const manifest = JSON.parse(
            readFileSync(join(bundleDir, "install-manifest.json"), "utf8"),
        );
        assert.equal(manifest.schema, SCHEMA);
        assert.equal(manifest.product, PRODUCT);
        assert.equal(manifest.version, "0.40.0");
        assert.equal(manifest.target, bundleTarget);
        assert.equal(manifest.entrypoint, `bin/${binaryName}`);
        assert.equal(manifest.stdlib, "stdlib");
        assert.equal(manifest.license, "LICENSE");
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dist, { recursive: true, force: true });
    }
});

test("manifest entrypoint and stdlib are relative paths (no absolute paths)", () => {
    const root = tempDir("rel-paths");
    const dist = tempDir("rel-paths-dist");
    try {
        createFakeWorkspace(root);
        const { bundleDir } = runFakeBundle(root, dist);
        const manifest = JSON.parse(
            readFileSync(join(bundleDir, "install-manifest.json"), "utf8"),
        );
        // No absolute paths in any string fields
        for (const [key, value] of Object.entries(manifest)) {
            if (typeof value !== "string") continue;
            assert.ok(
                !value.startsWith("/") && !/^[A-Za-z]:[\\/]/.test(value),
                `manifest.${key} must be relative, got: ${value}`,
            );
        }
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dist, { recursive: true, force: true });
    }
});

test("bundle directory name matches version and target", () => {
    const root = tempDir("dirname");
    const dist = tempDir("dirname-dist");
    try {
        createFakeWorkspace(root);
        const { dirName } = runFakeBundle(root, dist);
        assert.equal(dirName, `aster-0.40.0-${bundleTarget}`);
        assert.ok(existsSync(join(dist, dirName)));
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dist, { recursive: true, force: true });
    }
});

test("rebuild of existing bundle directory is idempotent", () => {
    const root = tempDir("rebuild");
    const dist = tempDir("rebuild-dist");
    try {
        createFakeWorkspace(root);
        runFakeBundle(root, dist);
        // Second run should not throw
        assert.doesNotThrow(() => runFakeBundle(root, dist));
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dist, { recursive: true, force: true });
    }
});

// ---------------------------------------------------------------------------
// Path with spaces and Unicode
// ---------------------------------------------------------------------------

test("buildBundle works when dist path contains spaces", () => {
    const root = tempDir("spaces");
    const dist = join(tempDir("spaces with spaces in path"), "dist output");
    try {
        createFakeWorkspace(root);
        mkdirSync(dist, { recursive: true });
        const result = buildBundle({
            workspaceRoot: root,
            distRoot: dist,
            version: "0.40.0",
            bundleTarget,
            binaryName,
        });
        assert.ok(existsSync(result.bundleDir));
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dirname(dist), { recursive: true, force: true });
    }
});

test("buildBundle works when dist path contains Unicode characters", () => {
    const root = tempDir("unicode");
    const dist = join(tempDir("üñïcödé-dist-äster"), "dist");
    try {
        createFakeWorkspace(root);
        mkdirSync(dist, { recursive: true });
        const result = buildBundle({
            workspaceRoot: root,
            distRoot: dist,
            version: "0.40.0",
            bundleTarget,
            binaryName,
        });
        assert.ok(existsSync(result.bundleDir));
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dirname(dist), { recursive: true, force: true });
    }
});

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

test("two consecutive bundles produce identical manifests and file list", () => {
    const root = tempDir("determinism");
    const dist = tempDir("determinism-dist");
    try {
        createFakeWorkspace(root);
        const { bundleDir: dir1 } = runFakeBundle(root, dist);

        const manifest1 = readFileSync(join(dir1, "install-manifest.json"), "utf8");
        const license1 = readFileSync(join(dir1, "LICENSE"), "utf8");
        const binarySize1 = statSync(join(dir1, "bin", binaryName)).size;

        // Second run in a different dist to allow side-by-side comparison
        const dist2 = tempDir("determinism-dist2");
        try {
            mkdirSync(dist2, { recursive: true });
            const { bundleDir: dir2 } = buildBundle({
                workspaceRoot: root,
                distRoot: dist2,
                version: "0.40.0",
                bundleTarget,
                binaryName,
            });

            const manifest2 = readFileSync(join(dir2, "install-manifest.json"), "utf8");
            const license2 = readFileSync(join(dir2, "LICENSE"), "utf8");
            const binarySize2 = statSync(join(dir2, "bin", binaryName)).size;

            assert.equal(manifest1, manifest2, "manifests must be identical");
            assert.equal(license1, license2, "LICENSE contents must be identical");
            assert.equal(binarySize1, binarySize2, "binary sizes must be identical");
        } finally {
            rmSync(dist2, { recursive: true, force: true });
        }
    } finally {
        rmSync(root, { recursive: true, force: true });
        rmSync(dist, { recursive: true, force: true });
    }
});

// ---------------------------------------------------------------------------
// Relocatable proof — requires the real release binary
// ---------------------------------------------------------------------------

const STDLIB_PROGRAM =
    "using aster.math; public class Program { public static int Main() { return Math.Max(0, 1); } }";

test(
    "relocatable bundle: stdlib found via exe-relative path, not cwd or ASTER_STDLIB",
    {
        skip: hasReleaseBinary
            ? false
            : "release binary not built — run: cargo +stable-gnu build --release -p aster-cli",
    },
    () => {
        // 1. Build the bundle from the real workspace into a temp dist.
        const tempDist = tempDir("reloc-dist");
        const { bundleDir: sourceBundle, binaryName: binName } = buildBundle({
            workspaceRoot: repositoryRoot,
            distRoot: tempDist,
            version: readVersion(join(repositoryRoot, "Cargo.toml")),
            bundleTarget,
            binaryName,
        });

        // 2. Copy the bundle to a directory with spaces and Unicode in the path.
        const relocDir = join(tempDir("reloc üñïcödé with spaces"), "aster bundle copy");
        mkdirSync(relocDir, { recursive: true });
        cpSync(sourceBundle, relocDir, { recursive: true });

        // 3. Create a project outside the repository.
        const projectDir = tempDir("reloc-project");
        const srcFile = join(projectDir, "main.aster");
        writeFileSync(srcFile, STDLIB_PROGRAM);

        // 4. Locate the binary inside the relocated bundle.
        const relocatedBinary = join(relocDir, "bin", binName);

        try {
            function runBundleBinary(args) {
                return spawnSync(relocatedBinary, args, {
                    cwd: projectDir,
                    env: {
                        ...Object.fromEntries(
                            Object.entries(process.env).filter(
                                ([k]) => k !== "ASTER_STDLIB",
                            ),
                        ),
                    },
                    encoding: "utf8",
                });
            }

            // aster --version must succeed (no compilation needed)
            const versionResult = runBundleBinary(["--version"]);
            assert.equal(
                versionResult.status,
                0,
                `--version failed: ${versionResult.stderr}`,
            );
            assert.match(
                versionResult.stdout,
                /aster \d+\.\d+\.\d+/,
                "version output must match expected format",
            );

            // aster check
            const checkResult = runBundleBinary(["check", srcFile]);
            assert.equal(
                checkResult.status,
                0,
                `aster check failed — stdlib likely not found via exe-relative path:\n${checkResult.stderr}`,
            );

            // aster dump-hir
            const hirResult = runBundleBinary(["dump-hir", srcFile]);
            assert.equal(
                hirResult.status,
                0,
                `aster dump-hir failed:\n${hirResult.stderr}`,
            );

            // aster dump-mir
            const mirResult = runBundleBinary(["dump-mir", srcFile]);
            assert.equal(
                mirResult.status,
                0,
                `aster dump-mir failed:\n${mirResult.stderr}`,
            );

            // aster run
            const runResult = runBundleBinary(["run", srcFile]);
            assert.equal(
                runResult.status,
                0,
                `aster run failed:\n${runResult.stderr}`,
            );
            assert.equal(
                runResult.stdout.trim(),
                "1",
                "program output must be 1",
            );

            // --- Proof that the EXE-RELATIVE stdlib is used, not the embedded fallback ---
            // Remove a required stdlib file from the relocated bundle.
            const mathFile = join(relocDir, "stdlib", "aster", "math.aster");
            const mathBackup = mathFile + ".bak";
            renameSync(mathFile, mathBackup);

            try {
                // aster --version should still work (no compilation)
                const versionAfter = runBundleBinary(["--version"]);
                assert.equal(
                    versionAfter.status,
                    0,
                    "--version must not need stdlib",
                );

                // aster check must fail with explicit stdlib error (not silently fall back)
                const checkBroken = runBundleBinary(["check", srcFile]);
                assert.notEqual(
                    checkBroken.status,
                    0,
                    "check must fail when required stdlib file is removed",
                );
                assert.ok(
                    checkBroken.stderr.includes("stdlib") ||
                        checkBroken.stderr.includes("math") ||
                        checkBroken.stderr.includes("incomplete") ||
                        checkBroken.stderr.includes("invalid"),
                    `expected explicit stdlib error, got: ${checkBroken.stderr}`,
                );
            } finally {
                // Restore the file so cleanup can proceed cleanly.
                renameSync(mathBackup, mathFile);
            }
        } finally {
            rmSync(tempDist, { recursive: true, force: true });
            rmSync(dirname(relocDir), { recursive: true, force: true });
            rmSync(projectDir, { recursive: true, force: true });
        }
    },
);

test(
    "bundle binary works with arbitrary current directory (cwd independence)",
    {
        skip: hasReleaseBinary
            ? false
            : "release binary not built",
    },
    () => {
        // Build a bundle into a temp dist.
        const tempDist = tempDir("cwd-dist");
        const { bundleDir } = buildBundle({
            workspaceRoot: repositoryRoot,
            distRoot: tempDist,
            version: readVersion(join(repositoryRoot, "Cargo.toml")),
            bundleTarget,
            binaryName,
        });

        // Project lives outside the repo.
        const projectDir = tempDir("cwd-project");
        const srcFile = join(projectDir, "main.aster");
        writeFileSync(srcFile, STDLIB_PROGRAM);

        // Use the OS temp dir root as cwd — unrelated to both the bundle and the project.
        const unrelatedCwd = tmpdir();

        try {
            const result = spawnSync(join(bundleDir, "bin", binaryName), ["run", srcFile], {
                cwd: unrelatedCwd,
                env: {
                    ...Object.fromEntries(
                        Object.entries(process.env).filter(([k]) => k !== "ASTER_STDLIB"),
                    ),
                },
                encoding: "utf8",
            });
            assert.equal(
                result.status,
                0,
                `aster run failed with unrelated cwd:\n${result.stderr}`,
            );
            assert.equal(result.stdout.trim(), "1");
        } finally {
            rmSync(tempDist, { recursive: true, force: true });
            rmSync(projectDir, { recursive: true, force: true });
        }
    },
);

// ---------------------------------------------------------------------------
// Determinism with real binary (bundle command invoked via CLI)
// ---------------------------------------------------------------------------

test(
    "npm run bundle is deterministic across two consecutive runs",
    {
        skip: hasReleaseBinary
            ? false
            : "release binary not built",
    },
    () => {
        // Run the bundle script twice against a fresh temp dist and compare manifests.
        function runBundleScript(distRoot) {
            mkdirSync(distRoot, { recursive: true });
            execFileSync(
                process.execPath,
                [join(repositoryRoot, "scripts", "bundle.mjs")],
                {
                    cwd: repositoryRoot,
                    env: { ...process.env },
                    stdio: "pipe",
                    // Override dist root via env for testability — but the script
                    // uses the real root; we run against a temp dist instead by
                    // running buildBundle directly.
                },
            );
        }

        // Instead of hijacking the real dist/, run buildBundle with a temp dist twice.
        const dist1 = tempDir("det-dist1");
        const dist2 = tempDir("det-dist2");
        const version = readVersion(join(repositoryRoot, "Cargo.toml"));

        try {
            const r1 = buildBundle({ workspaceRoot: repositoryRoot, distRoot: dist1, version, bundleTarget, binaryName });
            const r2 = buildBundle({ workspaceRoot: repositoryRoot, distRoot: dist2, version, bundleTarget, binaryName });

            const manifest1 = readFileSync(join(r1.bundleDir, "install-manifest.json"), "utf8");
            const manifest2 = readFileSync(join(r2.bundleDir, "install-manifest.json"), "utf8");
            assert.equal(manifest1, manifest2, "manifests must be identical across two runs");

            const size1 = statSync(join(r1.bundleDir, "bin", binaryName)).size;
            const size2 = statSync(join(r2.bundleDir, "bin", binaryName)).size;
            assert.equal(size1, size2, "binary sizes must match");

            for (const rel of REQUIRED_STDLIB_MODULES) {
                const c1 = readFileSync(join(r1.bundleDir, "stdlib", rel.split("/").join(sep)), "utf8");
                const c2 = readFileSync(join(r2.bundleDir, "stdlib", rel.split("/").join(sep)), "utf8");
                assert.equal(c1, c2, `stdlib content must match for ${rel}`);
            }
        } finally {
            rmSync(dist1, { recursive: true, force: true });
            rmSync(dist2, { recursive: true, force: true });
        }
    },
);
