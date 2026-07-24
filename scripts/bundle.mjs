#!/usr/bin/env node
//! Bundle tool for ASTER install distributions.
//!
//! Assembles a self-contained install bundle:
//!   dist/aster-{version}-{target}/
//!     bin/aster[.exe]
//!     stdlib/aster/
//!     LICENSE
//!     install-manifest.json
//!
//! Priority for stdlib at runtime (M6A1):
//!   ASTER_STDLIB  →  <exe-dir>/../stdlib/  →  embedded

import {
    chmodSync,
    copyFileSync,
    cpSync,
    existsSync,
    mkdirSync,
    readFileSync,
    readdirSync,
    rmSync,
    statSync,
    writeFileSync,
} from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

export const PRODUCT = "aster";
export const SCHEMA = 1;

export const REQUIRED_STDLIB_MODULES = [
    "aster/math.aster",
    "aster/text/text.aster",
    "aster/core/core.aster",
    "aster/io/io.aster",
    "aster/collections/collections.aster",
];

const SUPPORTED_TARGETS = {
    "win32/x64": { bundleTarget: "windows-x64", binaryName: "aster.exe" },
    "linux/x64": { bundleTarget: "linux-x64", binaryName: "aster" },
};

/** Detect bundle target from the running process platform and architecture. */
export function detectBundleTarget() {
    const key = `${process.platform}/${process.arch}`;
    const entry = SUPPORTED_TARGETS[key];
    if (!entry) {
        throw new Error(
            `Unsupported platform for bundling: ${process.platform}/${process.arch}\n` +
                `Supported: windows-x64 (win32/x64), linux-x64 (linux/x64)`,
        );
    }
    return { ...entry };
}

/** Read [workspace.package].version from Cargo.toml. */
export function readVersion(cargoTomlPath) {
    const content = readFileSync(cargoTomlPath, "utf8");
    const match = content.match(
        /\[workspace\.package\][\s\S]*?^\s*version\s*=\s*"([^"]+)"/m,
    );
    if (!match) {
        throw new Error(
            `Could not read [workspace.package].version from ${cargoTomlPath}`,
        );
    }
    return match[1];
}

/** Canonical bundle directory name, e.g. "aster-0.40.0-windows-x64". */
export function bundleDirectoryName(version, bundleTarget) {
    return `${PRODUCT}-${version}-${bundleTarget}`;
}

/** Try to get the Rust host triple from `rustc -vV`. Returns null on failure. */
export function tryGetRustTarget() {
    try {
        const output = execFileSync("rustc", ["-vV"], {
            encoding: "utf8",
            stdio: ["pipe", "pipe", "pipe"],
        });
        return output.match(/^host:\s+(\S+)$/m)?.[1] ?? null;
    } catch {
        return null;
    }
}

/**
 * Assert bundleDir is directly inside distDir with no path-escape via "..".
 * Also asserts the expected directory name matches when provided.
 */
export function assertSafeBundle(distDir, bundleDir, expectedName) {
    if (!bundleDir || bundleDir === sep || bundleDir === "/") {
        throw new Error("Bundle path is empty or the filesystem root — aborting");
    }
    const rel = relative(distDir, bundleDir);
    if (!rel || rel.startsWith("..") || rel.includes("..")) {
        throw new Error(
            `Bundle directory is not safely inside dist/ — aborting\n` +
                `  distDir:   ${distDir}\n` +
                `  bundleDir: ${bundleDir}`,
        );
    }
    if (expectedName !== undefined && rel !== expectedName) {
        throw new Error(
            `Bundle directory name mismatch: expected "${expectedName}", got "${rel}"`,
        );
    }
}

/**
 * Assemble the install bundle.
 *
 * @param {object} opts
 * @param {string} opts.workspaceRoot  Absolute path to the repository root.
 * @param {string} opts.distRoot       Absolute path to the dist/ output directory.
 * @param {string} opts.version        Semantic version string (e.g. "0.40.0").
 * @param {string} opts.bundleTarget   Public target name (e.g. "windows-x64").
 * @param {string} opts.binaryName     Binary filename (e.g. "aster.exe").
 * @param {string} [opts.rustTarget]   Rust triple recorded in the manifest.
 * @returns {{ bundleDir: string, dirName: string, version: string, bundleTarget: string, binaryName: string }}
 */
export function buildBundle({
    workspaceRoot,
    distRoot,
    version,
    bundleTarget,
    binaryName,
    rustTarget,
}) {
    const dirName = bundleDirectoryName(version, bundleTarget);
    const bundleDir = join(distRoot, dirName);

    assertSafeBundle(distRoot, bundleDir, dirName);

    // Source paths
    const binarySource = join(workspaceRoot, "target", "release", binaryName);
    const stdlibSource = join(workspaceRoot, "stdlib");
    const licenseSource = join(workspaceRoot, "LICENSE");

    // Validate all sources exist before touching the output directory.
    if (!existsSync(binarySource) || !statSync(binarySource).isFile()) {
        throw new Error(
            `Release binary not found: ${binarySource}\n` +
                `Run: cargo +stable-gnu build --release -p aster-cli`,
        );
    }
    if (!existsSync(licenseSource) || !statSync(licenseSource).isFile()) {
        throw new Error(`LICENSE file not found: ${licenseSource}`);
    }
    if (!existsSync(stdlibSource) || !statSync(stdlibSource).isDirectory()) {
        throw new Error(`stdlib directory not found: ${stdlibSource}`);
    }
    for (const rel of REQUIRED_STDLIB_MODULES) {
        const full = join(stdlibSource, rel.split("/").join(sep));
        if (!existsSync(full)) {
            throw new Error(`Required stdlib module missing: ${rel}`);
        }
    }

    // Remove only the specific bundle directory (safe: asserted above).
    if (existsSync(bundleDir)) {
        assertSafeBundle(distRoot, bundleDir, dirName);
        rmSync(bundleDir, { recursive: true, force: true });
    }

    // Create the layout.
    const binDir = join(bundleDir, "bin");
    mkdirSync(binDir, { recursive: true });

    // Copy binary (and set executable bit on non-Windows).
    const binaryDest = join(binDir, binaryName);
    copyFileSync(binarySource, binaryDest);
    if (process.platform !== "win32") {
        chmodSync(binaryDest, 0o755);
    }

    // Copy the entire stdlib tree.
    cpSync(stdlibSource, join(bundleDir, "stdlib"), { recursive: true });

    // Copy the license.
    copyFileSync(licenseSource, join(bundleDir, "LICENSE"));

    // Generate a deterministic install-manifest.json.
    const manifest = {
        schema: SCHEMA,
        product: PRODUCT,
        version,
        target: bundleTarget,
        entrypoint: `bin/${binaryName}`, // forward-slash, always relative
        stdlib: "stdlib",
        license: "LICENSE",
    };
    if (rustTarget) {
        manifest.rustTarget = rustTarget;
    }
    writeFileSync(
        join(bundleDir, "install-manifest.json"),
        JSON.stringify(manifest, null, 2) + "\n",
        "utf8",
    );

    // Validate the completed bundle before reporting success.
    validateBundle(bundleDir, { version, bundleTarget, binaryName });

    return { bundleDir, dirName, version, bundleTarget, binaryName };
}

/**
 * Validate an assembled bundle directory.
 * Throws on any violation.
 */
export function validateBundle(bundleDir, { version, bundleTarget, binaryName } = {}) {
    function check(condition, message) {
        if (!condition) throw new Error(`Bundle validation failed: ${message}`);
    }

    // Binary
    if (binaryName) {
        const binaryPath = join(bundleDir, "bin", binaryName);
        check(
            existsSync(binaryPath) && statSync(binaryPath).isFile(),
            `binary not found at bin/${binaryName}`,
        );
    }

    // No .pdb files in bin/
    const binDir = join(bundleDir, "bin");
    if (existsSync(binDir)) {
        for (const entry of readdirSync(binDir)) {
            check(!entry.endsWith(".pdb"), `unexpected .pdb file in bin/: ${entry}`);
        }
    }

    // stdlib/aster/ directory
    const stdlibAster = join(bundleDir, "stdlib", "aster");
    check(
        existsSync(stdlibAster) && statSync(stdlibAster).isDirectory(),
        "stdlib/aster/ directory not found",
    );

    // Required stdlib modules
    for (const rel of REQUIRED_STDLIB_MODULES) {
        const full = join(bundleDir, "stdlib", rel.split("/").join(sep));
        check(existsSync(full), `required stdlib module missing from bundle: ${rel}`);
    }

    // LICENSE
    check(existsSync(join(bundleDir, "LICENSE")), "LICENSE not found");

    // install-manifest.json
    const manifestPath = join(bundleDir, "install-manifest.json");
    check(existsSync(manifestPath), "install-manifest.json not found");

    let manifest;
    try {
        manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    } catch {
        throw new Error("Bundle validation failed: install-manifest.json is not valid JSON");
    }

    check(manifest.schema === SCHEMA, `manifest schema must be ${SCHEMA}, got ${manifest.schema}`);
    check(manifest.product === PRODUCT, `manifest product must be "${PRODUCT}"`);
    if (version) check(manifest.version === version, `manifest version "${manifest.version}" != expected "${version}"`);
    if (bundleTarget) check(manifest.target === bundleTarget, `manifest target "${manifest.target}" != expected "${bundleTarget}"`);

    // Entrypoint must be relative (forward-slash, no drive letters or leading slash)
    const ep = manifest.entrypoint;
    check(typeof ep === "string", "manifest entrypoint must be a string");
    check(!ep.startsWith("/"), "manifest entrypoint must not start with /");
    check(!/^[A-Za-z]:[\\/]/.test(ep), "manifest entrypoint must not be an absolute Windows path");
    check(!ep.includes(".."), "manifest entrypoint must not contain ..");

    if (binaryName) {
        check(existsSync(join(bundleDir, ep.split("/").join(sep))), `manifest entrypoint does not exist in bundle: ${ep}`);
    }

    // Stdlib and license paths must be relative
    check(!manifest.stdlib.startsWith("/"), "manifest stdlib must be relative");
    check(!manifest.license.startsWith("/"), "manifest license must be relative");

    // No .git or target/ contamination
    check(!existsSync(join(bundleDir, ".git")), "bundle must not contain .git");
    check(!existsSync(join(bundleDir, "target")), "bundle must not contain target/");
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

const isMain = process.argv[1] === fileURLToPath(import.meta.url);

if (isMain) {
    const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
    const distRoot = join(repositoryRoot, "dist");

    try {
        const version = readVersion(join(repositoryRoot, "Cargo.toml"));
        const { bundleTarget, binaryName } = detectBundleTarget();

        mkdirSync(distRoot, { recursive: true });

        const { bundleDir } = buildBundle({
            workspaceRoot: repositoryRoot,
            distRoot,
            version,
            bundleTarget,
            binaryName,
        });

        const entrypoint = join("bin", binaryName);

        console.log(`\nASTER install bundle created\n`);
        console.log(`Version:          ${version}`);
        console.log(`Target:           ${bundleTarget}`);
        console.log(`Path:             ${bundleDir}`);
        console.log(`Entrypoint:       ${entrypoint}`);
        console.log(`Standard library: stdlib`);
        // rustTarget omitted: requires knowing the exact toolchain used for the build
    } catch (error) {
        console.error(`error: ${error.message}`);
        process.exit(1);
    }
}
