#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";
import {
    existsSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    readdirSync,
    rmSync,
    statSync,
    symlinkSync,
    writeFileSync,
} from "node:fs";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { basename, dirname, join, parse } from "node:path";
import { fileURLToPath } from "node:url";
import { once } from "node:events";

import {
    decodeArchiveBuffer,
    encodeZipEntries,
    sha256,
} from "./package-release.mjs";

const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const installPowerShellPath = join(repositoryRoot, "install", "install.ps1");
const installShellPath = join(repositoryRoot, "install", "install.sh");
const uninstallPowerShellPath = join(repositoryRoot, "install", "uninstall.ps1");
const uninstallShellPath = join(repositoryRoot, "install", "uninstall.sh");
const powerShellExecutable =
    "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
const shellExecutable = "C:\\Program Files\\Git\\bin\\sh.exe";
const artifactManifestPath = join(
    repositoryRoot,
    "dist",
    "artifacts",
    "release-artifact.json",
);
const hostSupportsWindowsInstaller =
    process.platform === "win32" &&
    process.arch === "x64" &&
    existsSync(powerShellExecutable);
const hostHasTestShell = existsSync(shellExecutable);

const realArtifact = (() => {
    if (!existsSync(artifactManifestPath)) return null;
    const manifest = JSON.parse(readFileSync(artifactManifestPath, "utf8"));
    if (manifest.target !== "windows-x64") return null;
    const archivePath = join(dirname(artifactManifestPath), manifest.archive);
    if (!existsSync(archivePath)) return null;
    const archive = readFileSync(archivePath);
    return {
        manifest,
        archive,
        entries: decodeArchiveBuffer(archive, manifest.target),
        checksum: `${sha256(archive)}  ${manifest.archive}\n`,
    };
})();

function temporaryDirectory(label) {
    return mkdtempSync(join(tmpdir(), `aster-installer-${label}-`));
}

function cloneEntries(entries) {
    return entries.map((entry) => ({
        ...entry,
        data: Buffer.from(entry.data),
    }));
}

function replaceEntry(entries, suffix, data) {
    const copy = cloneEntries(entries);
    const entry = copy.find((candidate) => candidate.name.endsWith(suffix));
    assert.ok(entry, `fixture entry not found: ${suffix}`);
    entry.data = Buffer.isBuffer(data) ? data : Buffer.from(data, "utf8");
    return copy;
}

function removeEntry(entries, suffix) {
    return cloneEntries(entries).filter((entry) => !entry.name.endsWith(suffix));
}

function archiveFixture(entries) {
    const archive = encodeZipEntries(entries);
    return {
        archive,
        checksum: `${sha256(archive)}  ignored-versioned-name.zip\n`,
    };
}

async function startInstallerServer({
    archive,
    checksum,
    archiveStatus = 200,
    checksumStatus = 200,
    archiveHeaders = {},
    checksumHeaders = {},
    disconnectArchive = false,
    redirectArchive = false,
} = {}) {
    const requests = [];
    const installerScript = readFileSync(installPowerShellPath);
    const uninstallerScript = readFileSync(uninstallPowerShellPath);
    const server = createServer((request, response) => {
        requests.push(request.url);
        if (request.url === "/install.ps1") {
            response.writeHead(200, { "Content-Type": "text/plain" });
            response.end(installerScript);
            return;
        }
        if (request.url === "/uninstall.ps1") {
            response.writeHead(200, { "Content-Type": "text/plain" });
            response.end(uninstallerScript);
            return;
        }
        if (request.url === "/aster-windows-x64.zip" && redirectArchive) {
            response.writeHead(302, { Location: "/payload/archive.zip" });
            response.end();
            return;
        }
        if (
            request.url === "/aster-windows-x64.zip" ||
            request.url === "/payload/archive.zip"
        ) {
            if (disconnectArchive) {
                response.writeHead(200);
                response.write(Buffer.from("partial"));
                response.destroy();
                return;
            }
            response.writeHead(archiveStatus, {
                "Content-Type": "application/zip",
                ...archiveHeaders,
            });
            response.end(archiveStatus === 200 ? archive : "archive error");
            return;
        }
        if (request.url === "/aster-windows-x64.zip.sha256") {
            response.writeHead(checksumStatus, {
                "Content-Type": "text/plain",
                ...checksumHeaders,
            });
            response.end(checksumStatus === 200 ? checksum : "checksum error");
            return;
        }
        response.writeHead(404);
        response.end("not found");
    });
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const address = server.address();
    return {
        server,
        requests,
        baseUrl: `http://127.0.0.1:${address.port}`,
        close: async () => {
            server.close();
            await once(server, "close");
        },
    };
}

function runPowerShellInstaller({
    scriptUrl,
    baseUrl,
    installDirectory,
    allowInsecure = true,
    extraEnvironment = {},
    cwd,
}) {
    const command =
        `$ProgressPreference = 'SilentlyContinue'; ` +
        `Invoke-RestMethod -UseBasicParsing '${scriptUrl}' | Invoke-Expression`;
    const environment = {
        ...process.env,
        ASTER_INSTALL_BASE_URL: baseUrl,
        ASTER_INSTALL_DIR: installDirectory,
        ASTER_INSTALL_SKIP_PATH: "1",
        ASTER_STDLIB: join(tmpdir(), "aster-installer-bogus-stdlib-missing"),
        ...extraEnvironment,
    };
    if (allowInsecure) environment.ASTER_INSTALL_ALLOW_INSECURE = "1";
    else delete environment.ASTER_INSTALL_ALLOW_INSECURE;

    return new Promise((resolve, reject) => {
        const child = spawn(
            powerShellExecutable,
            [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                command,
            ],
            {
                cwd: cwd ?? tmpdir(),
                env: environment,
                windowsHide: true,
            },
        );
        let stdout = "";
        let stderr = "";
        const timer = setTimeout(() => {
            child.kill();
            reject(new Error("PowerShell installer timed out"));
        }, 30_000);
        child.stdout.setEncoding("utf8");
        child.stderr.setEncoding("utf8");
        child.stdout.on("data", (chunk) => {
            stdout += chunk;
        });
        child.stderr.on("data", (chunk) => {
            stderr += chunk;
        });
        child.on("error", (error) => {
            clearTimeout(timer);
            reject(error);
        });
        child.on("close", (status) => {
            clearTimeout(timer);
            resolve({ status, stdout, stderr });
        });
    });
}

function runPowerShellUninstaller({ scriptUrl, installDirectory, extraEnvironment = {} }) {
    const command =
        `$ProgressPreference = 'SilentlyContinue'; ` +
        `Invoke-RestMethod -UseBasicParsing '${scriptUrl}' | Invoke-Expression`;
    return new Promise((resolve, reject) => {
        const child = spawn(
            powerShellExecutable,
            [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                command,
            ],
            {
                cwd: tmpdir(),
                env: {
                    ...process.env,
                    ASTER_INSTALL_DIR: installDirectory,
                    ASTER_INSTALL_SKIP_PATH: "1",
                    ...extraEnvironment,
                },
                windowsHide: true,
            },
        );
        let stdout = "";
        let stderr = "";
        child.stdout.setEncoding("utf8");
        child.stderr.setEncoding("utf8");
        child.stdout.on("data", (chunk) => {
            stdout += chunk;
        });
        child.stderr.on("data", (chunk) => {
            stderr += chunk;
        });
        child.on("error", reject);
        child.on("close", (status) => resolve({ status, stdout, stderr }));
    });
}

function runTestShell(script, environment = {}) {
    return new Promise((resolve, reject) => {
        const child = spawn(shellExecutable, ["-c", script], {
            env: { ...process.env, ...environment },
            windowsHide: true,
        });
        let stdout = "";
        let stderr = "";
        child.stdout.setEncoding("utf8");
        child.stderr.setEncoding("utf8");
        child.stdout.on("data", (chunk) => {
            stdout += chunk;
        });
        child.stderr.on("data", (chunk) => {
            stderr += chunk;
        });
        child.on("error", reject);
        child.on("close", (status) => resolve({ status, stdout, stderr }));
    });
}

function gitShellPath(path) {
    return path.replace(/^([A-Za-z]):/, (_, drive) => `/${drive.toLowerCase()}`).replaceAll("\\", "/");
}

function managedSiblingNames(parent, installDirectory) {
    const base = basename(installDirectory);
    return readdirSync(parent)
        .filter((name) => name.startsWith(`${base}.staging-`) || name.startsWith(`${base}.backup-`))
        .sort();
}

function snapshotTree(root) {
    const result = {};
    function visit(directory, prefix) {
        for (const entry of readdirSync(directory, { withFileTypes: true })) {
            const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
            const path = join(directory, entry.name);
            if (entry.isDirectory()) visit(path, relative);
            else result[relative] = sha256(readFileSync(path));
        }
    }
    visit(root, "");
    return result;
}

function writeManagedState(directory, version, target = "windows-x64") {
    writeFileSync(
        join(directory, "install-state.json"),
        JSON.stringify({ schema: 1, product: "aster", version, target }, null, 2) + "\n",
    );
}

async function withServer(options, callback) {
    const fixture = await startInstallerServer(options);
    try {
        return await callback(fixture);
    } finally {
        await fixture.close();
    }
}

function assertFailed(result, pattern) {
    assert.notEqual(result.status, 0, `installer unexpectedly succeeded:\n${result.stdout}`);
    assert.match(result.stderr, pattern);
    assert.ok(!result.stderr.includes(" at "), "expected failure must not print a stack trace");
}

test("installer scripts expose only the documented targets, aliases, and overrides", () => {
    const windows = readFileSync(installPowerShellPath, "utf8");
    const linux = readFileSync(installShellPath, "utf8");
    const windowsUninstall = readFileSync(uninstallPowerShellPath, "utf8");
    const linuxUninstall = readFileSync(uninstallShellPath, "utf8");
    assert.match(windows, /aster-windows-x64\.zip/);
    assert.match(linux, /aster-linux-x64\.tar\.gz/);
    for (const source of [windows, linux, windowsUninstall, linuxUninstall]) {
        assert.match(source, /ASTER_INSTALL_DIR/);
        assert.match(source, /ASTER_INSTALL_SKIP_PATH/);
        assert.ok(!source.includes("0.41.0"), "installer must not hardcode a release version");
        assert.ok(!/\beval\b/.test(source), "installer must not use eval");
    }
    for (const source of [windows, linux]) {
        assert.match(source, /ASTER_INSTALL_BASE_URL/);
        assert.match(source, /ASTER_INSTALL_ALLOW_INSECURE/);
    }
    assert.ok(!windows.includes("$PSScriptRoot"));
    assert.ok(!windowsUninstall.includes("$PSScriptRoot"));
    assert.ok(!windows.toLowerCase().includes("setx"));
    assert.ok(!windowsUninstall.toLowerCase().includes("setx"));
    assert.match(linux, /^#!\/bin\/sh\nset -eu/m);
    assert.match(linuxUninstall, /^#!\/bin\/sh\nset -eu/m);
    assert.doesNotMatch(linux, /^[ \t]*\[\[/m);
    assert.doesNotMatch(linuxUninstall, /^[ \t]*\[\[/m);
    assert.ok(!linux.includes("pipefail"));
    assert.ok(!linuxUninstall.includes("pipefail"));
    assert.match(linux, /# >>> ASTER installer >>>/);
    assert.match(linux, /# <<< ASTER installer <<</);
});

test("PowerShell PATH helper is case-insensitive, idempotent, and preserves other entries", async () => {
    if (!hostSupportsWindowsInstaller) return;
    const source = readFileSync(installPowerShellPath, "utf8");
    const marker = "\ntry {\n    Invoke-AsterInstall";
    const definitions = source.slice(0, source.lastIndexOf(marker));
    const testScript = `${definitions}
$existing = 'C:\\Tools;C:\\Aster\\bin'
$same = Get-UpdatedWindowsPath $existing 'c:\\aster\\bin\\'
if ($same -cne $existing) { throw 'duplicate PATH entry' }
$added = Get-UpdatedWindowsPath $existing 'C:\\New Aster\\bin'
if ($added -cne 'C:\\New Aster\\bin;C:\\Tools;C:\\Aster\\bin') { throw 'PATH preservation failed' }
$emptyRejected = $false
try { [void](Assert-SafeInstallDirectory '') }
catch {
    if ($_.Exception.Message -notmatch 'must not be empty') { throw }
    $emptyRejected = $true
}
if (-not $emptyRejected) { throw 'empty install directory was accepted' }
`;
    const child = spawn(powerShellExecutable, [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "-",
    ], { windowsHide: true });
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => {
        stderr += chunk;
    });
    child.stdin.end(testScript);
    const [status] = await once(child, "close");
    assert.equal(status, 0, stderr);
});

test("PowerShell uninstall PATH helper removes only exact ASTER entries", async () => {
    if (!hostSupportsWindowsInstaller) return;
    const source = readFileSync(uninstallPowerShellPath, "utf8");
    const marker = "\ntry {\n    Invoke-AsterUninstall";
    const definitions = source.slice(0, source.lastIndexOf(marker));
    const testScript = `${definitions}
$current = 'C:\\Aster\\bin;C:\\Tools;C:\\ASTER\\BIN\\;C:\\Aster\\binary'
$updated = Get-WindowsPathWithoutEntry $current 'c:\\aster\\bin'
if ($updated -cne 'C:\\Tools;C:\\Aster\\binary') { throw 'PATH removal failed' }
`;
    const child = spawn(powerShellExecutable, [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "-",
    ], { windowsHide: true });
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => {
        stderr += chunk;
    });
    child.stdin.end(testScript);
    const [status] = await once(child, "close");
    assert.equal(status, 0, stderr);
});

test("Linux profile helpers are idempotent, preserve surrounding text, and reject malformed blocks", async () => {
    if (!hostHasTestShell) return;
    const installSource = readFileSync(installShellPath, "utf8");
    const uninstallSource = readFileSync(uninstallShellPath, "utf8");
    const addDefinitions = installSource.slice(
        installSource.indexOf("escape_profile_path()"),
        installSource.indexOf("write_install_state()"),
    );
    const removeDefinitions = uninstallSource.slice(
        uninstallSource.indexOf("remove_path_block()"),
        uninstallSource.indexOf("if [ ! -e"),
    );
    const parent = temporaryDirectory("linux-profile");
    const environment = { HOME: gitShellPath(parent) };
    const profile = join(parent, ".profile");
    try {
        writeFileSync(profile, "before\n");
        const added = await runTestShell(
            `set -eu\nfail() { printf '%s\\n' "error: $*" >&2; exit 1; }\n${addDefinitions}\nadd_path_block "$HOME/custom bin"\nadd_path_block "$HOME/custom bin"\n`,
            environment,
        );
        assert.equal(added.status, 0, added.stderr);
        const installedProfile = readFileSync(profile, "utf8");
        assert.equal(
            installedProfile.match(/# >>> ASTER installer >>>/g)?.length,
            1,
        );
        assert.match(installedProfile, /^before$/m);

        const removed = await runTestShell(
            `set -eu\nfail() { printf '%s\\n' "error: $*" >&2; exit 1; }\n${removeDefinitions}\nremove_path_block\n`,
            environment,
        );
        assert.equal(removed.status, 0, removed.stderr);
        const removedProfile = readFileSync(profile, "utf8");
        assert.match(removedProfile, /^before$/m);
        assert.doesNotMatch(removedProfile, /ASTER installer/);
        assert.ok(existsSync(`${profile}.aster-backup`));

        writeFileSync(profile, "before\n# >>> ASTER installer >>>\n");
        const malformedBefore = readFileSync(profile, "utf8");
        const malformed = await runTestShell(
            `set -eu\nfail() { printf '%s\\n' "error: $*" >&2; exit 1; }\n${removeDefinitions}\nremove_path_block\n`,
            environment,
        );
        assert.notEqual(malformed.status, 0);
        assert.match(malformed.stderr, /incomplete or duplicated/);
        assert.equal(readFileSync(profile, "utf8"), malformedBefore);

        rmSync(profile);
        const skipped = await runTestShell(
            `set -eu\nfail() { exit 1; }\n${addDefinitions}\nadd_path_block "$HOME/skipped"\n`,
            { ...environment, ASTER_INSTALL_SKIP_PATH: "1" },
        );
        assert.equal(skipped.status, 0, skipped.stderr);
        assert.ok(!existsSync(profile));
    } finally {
        rmSync(parent, { recursive: true, force: true });
    }
});

test(
    "Windows installer installs from loopback into an empty Unicode path and validates the CLI",
    {
        skip:
            hostSupportsWindowsInstaller && realArtifact
                ? false
                : "Windows x64 M6A3 artifact is required",
    },
    async () => {
        const parent = temporaryDirectory("valid path with spaces ç");
        const installDirectory = join(parent, "instalação ASTER");
        mkdirSync(installDirectory);
        const unrelatedCwd = temporaryDirectory("unrelated-cwd");
        try {
            await withServer(realArtifact, async (server) => {
                const result = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                    cwd: unrelatedCwd,
                });
                assert.equal(result.status, 0, result.stderr);
                assert.match(result.stdout, /ASTER installed successfully/);
                assert.deepEqual(server.requests, [
                    "/install.ps1",
                    "/aster-windows-x64.zip",
                    "/aster-windows-x64.zip.sha256",
                ]);
            });
            const expected = [
                "LICENSE",
                "bin",
                "install-manifest.json",
                "install-state.json",
                "stdlib",
            ];
            assert.deepEqual(readdirSync(installDirectory).sort(), expected);
            const stateText = readFileSync(
                join(installDirectory, "install-state.json"),
                "utf8",
            );
            assert.ok(stateText.endsWith("\n"));
            const state = JSON.parse(stateText);
            assert.deepEqual(state, {
                schema: 1,
                product: "aster",
                version: realArtifact.manifest.version,
                target: "windows-x64",
            });
            assert.ok(!stateText.includes(parent));
            assert.ok(!stateText.includes("timestamp"));
        } finally {
            rmSync(parent, { recursive: true, force: true });
            rmSync(unrelatedCwd, { recursive: true, force: true });
        }
    },
);

test(
    "same healthy managed version is not replaced and leaves no staging or backup",
    {
        skip:
            hostSupportsWindowsInstaller && realArtifact
                ? false
                : "Windows x64 M6A3 artifact is required",
    },
    async () => {
        const parent = temporaryDirectory("same-version");
        const installDirectory = join(parent, "Aster");
        try {
            await withServer(realArtifact, async (server) => {
                const first = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assert.equal(first.status, 0, first.stderr);
                const binaryPath = join(installDirectory, "bin", "aster.exe");
                const statePath = join(installDirectory, "install-state.json");
                const before = {
                    binaryMtime: statSync(binaryPath).mtimeMs,
                    state: readFileSync(statePath, "utf8"),
                    tree: snapshotTree(installDirectory),
                };
                const second = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assert.equal(second.status, 0, second.stderr);
                assert.match(second.stdout, /already installed and healthy/);
                assert.equal(statSync(binaryPath).mtimeMs, before.binaryMtime);
                assert.equal(readFileSync(statePath, "utf8"), before.state);
                assert.deepEqual(snapshotTree(installDirectory), before.tree);
                assert.deepEqual(managedSiblingNames(parent, installDirectory), []);
            });
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "same-version damage is repaired atomically for binary, stdlib, and manifest failures",
    {
        skip:
            hostSupportsWindowsInstaller && realArtifact
                ? false
                : "Windows x64 M6A3 artifact is required",
    },
    async (context) => {
        const cases = [
            ["missing binary", (root) => rmSync(join(root, "bin", "aster.exe"))],
            [
                "incomplete stdlib",
                (root) => rmSync(join(root, "stdlib", "aster", "math.aster")),
            ],
            [
                "invalid manifest",
                (root) => writeFileSync(join(root, "install-manifest.json"), "{ invalid"),
            ],
        ];
        for (const [name, damage] of cases) {
            await context.test(name, async () => {
                const parent = temporaryDirectory(`repair-${name}`);
                const installDirectory = join(parent, "Aster");
                try {
                    await withServer(realArtifact, async (server) => {
                        const first = await runPowerShellInstaller({
                            scriptUrl: `${server.baseUrl}/install.ps1`,
                            baseUrl: server.baseUrl,
                            installDirectory,
                        });
                        assert.equal(first.status, 0, first.stderr);
                        damage(installDirectory);
                        const repaired = await runPowerShellInstaller({
                            scriptUrl: `${server.baseUrl}/install.ps1`,
                            baseUrl: server.baseUrl,
                            installDirectory,
                        });
                        assert.equal(repaired.status, 0, repaired.stderr);
                        assert.match(repaired.stdout, /ASTER repaired successfully/);
                    });
                    assert.equal(
                        JSON.parse(
                            readFileSync(join(installDirectory, "install-state.json"), "utf8"),
                        ).version,
                        realArtifact.manifest.version,
                    );
                    assert.deepEqual(managedSiblingNames(parent, installDirectory), []);
                } finally {
                    rmSync(parent, { recursive: true, force: true });
                }
            });
        }
    },
);

test(
    "a different managed version is replaced atomically without SemVer ordering",
    {
        skip:
            hostSupportsWindowsInstaller && realArtifact
                ? false
                : "Windows x64 M6A3 artifact is required",
    },
    async () => {
        const parent = temporaryDirectory("update");
        const installDirectory = join(parent, "Aster");
        try {
            await withServer(realArtifact, async (server) => {
                const first = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assert.equal(first.status, 0, first.stderr);
                writeManagedState(installDirectory, "0.0.0");
                writeFileSync(join(installDirectory, "LICENSE"), "old managed content\n");
                const updated = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assert.equal(updated.status, 0, updated.stderr);
                assert.match(updated.stdout, /ASTER updated successfully/);
                assert.match(updated.stdout, /Previous version: 0\.0\.0/);
            });
            assert.equal(
                JSON.parse(readFileSync(join(installDirectory, "install-state.json"), "utf8"))
                    .version,
                realArtifact.manifest.version,
            );
            assert.notEqual(
                readFileSync(join(installDirectory, "LICENSE"), "utf8"),
                "old managed content\n",
            );
            assert.deepEqual(managedSiblingNames(parent, installDirectory), []);
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "pre-publication checksum failure preserves the managed installation byte for byte",
    {
        skip:
            hostSupportsWindowsInstaller && realArtifact
                ? false
                : "Windows x64 M6A3 artifact is required",
    },
    async () => {
        const parent = temporaryDirectory("pre-publication");
        const installDirectory = join(parent, "Aster");
        try {
            await withServer(realArtifact, async (server) => {
                const installed = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assert.equal(installed.status, 0, installed.stderr);
            });
            const before = snapshotTree(installDirectory);
            await withServer(
                { ...realArtifact, checksum: `${"0".repeat(64)}  archive.zip\n` },
                async (server) => {
                    const failed = await runPowerShellInstaller({
                        scriptUrl: `${server.baseUrl}/install.ps1`,
                        baseUrl: server.baseUrl,
                        installDirectory,
                    });
                    assertFailed(failed, /SHA-256/);
                },
            );
            assert.deepEqual(snapshotTree(installDirectory), before);
            assert.deepEqual(managedSiblingNames(parent, installDirectory), []);
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test("atomic replacement restores the previous managed directory after final validation fails", async () => {
    if (!hostSupportsWindowsInstaller) return;
    const parent = temporaryDirectory("rollback");
    const installDirectory = join(parent, "Aster");
    const stagingDirectory = `${installDirectory}.staging-controlled`;
    mkdirSync(installDirectory);
    mkdirSync(stagingDirectory);
    writeManagedState(installDirectory, "0.0.0");
    writeFileSync(join(installDirectory, "old.txt"), "old");
    writeFileSync(join(stagingDirectory, "new.txt"), "new");
    const source = readFileSync(installPowerShellPath, "utf8");
    const marker = "\ntry {\n    Invoke-AsterInstall";
    const definitions = source.slice(0, source.lastIndexOf(marker));
    const quote = (value) => value.replaceAll("'", "''");
    const testScript = `${definitions}
$install = '${quote(installDirectory)}'
$staging = '${quote(stagingDirectory)}'
$previous = Get-InstallDirectoryState $install
$validator = {
    param($path, $version, $phase)
    if ($phase -eq 'Final') { throw 'forced final validation failure' }
}
try {
    Publish-ManagedReplacement $install $staging '1.0.0' $previous.State $false $validator
    throw 'replacement unexpectedly succeeded'
}
catch {
    if ($_.Exception.Message -notmatch 'forced final validation failure.*previous installation was restored') {
        throw ('unexpected replacement error: ' + $_.Exception.Message)
    }
}
if (-not (Test-Path -LiteralPath (Join-Path $install 'old.txt'))) { throw 'old content missing' }
if (Test-Path -LiteralPath (Join-Path $install 'new.txt')) { throw 'new content survived rollback' }
`;
    try {
        const child = spawn(powerShellExecutable, [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "-",
        ], { windowsHide: true });
        let stderr = "";
        child.stderr.setEncoding("utf8");
        child.stderr.on("data", (chunk) => {
            stderr += chunk;
        });
        child.stdin.end(testScript);
        const [status] = await once(child, "close");
        assert.equal(status, 0, stderr);
        rmSync(stagingDirectory, { recursive: true, force: true });
        assert.deepEqual(managedSiblingNames(parent, installDirectory), []);
    } finally {
        rmSync(parent, { recursive: true, force: true });
    }
});

test(
    "HTTP redirect is followed and malicious checksum filename cannot control the local path",
    {
        skip:
            hostSupportsWindowsInstaller && realArtifact
                ? false
                : "Windows x64 M6A3 artifact is required",
    },
    async () => {
        const parent = temporaryDirectory("redirect");
        const installDirectory = join(parent, "Aster");
        const checksum = `${sha256(realArtifact.archive)}  ../../outside.exe\n`;
        try {
            await withServer(
                { ...realArtifact, checksum, redirectArchive: true },
                async (server) => {
                    const result = await runPowerShellInstaller({
                        scriptUrl: `${server.baseUrl}/install.ps1`,
                        baseUrl: server.baseUrl,
                        installDirectory,
                    });
                    assert.equal(result.status, 0, result.stderr);
                    assert.ok(server.requests.includes("/payload/archive.zip"));
                },
            );
            assert.ok(!existsSync(join(parent, "outside.exe")));
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "installer rejects HTTP without test flag and rejects URL credentials before archive download",
    { skip: hostSupportsWindowsInstaller ? false : "Windows x64 required" },
    async (context) => {
        const parent = temporaryDirectory("url-security");
        try {
            await withServer(
                {
                    archive: realArtifact?.archive ?? Buffer.from("archive"),
                    checksum: realArtifact?.checksum ?? "0".repeat(64) + "  archive\n",
                },
                async (server) => {
                    await context.test("HTTP without flag", async () => {
                        const result = await runPowerShellInstaller({
                            scriptUrl: `${server.baseUrl}/install.ps1`,
                            baseUrl: server.baseUrl,
                            installDirectory: join(parent, "http"),
                            allowInsecure: false,
                        });
                        assertFailed(result, /must use HTTPS/);
                    });
                    await context.test("credentials", async () => {
                        const credentialUrl = server.baseUrl.replace(
                            "http://",
                            "http://user:secret@",
                        );
                        const result = await runPowerShellInstaller({
                            scriptUrl: `${server.baseUrl}/install.ps1`,
                            baseUrl: credentialUrl,
                            installDirectory: join(parent, "credentials"),
                        });
                        assertFailed(result, /must not contain credentials/);
                        assert.ok(!result.stderr.includes("secret@"));
                    });
                },
            );
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "download failures, empty responses, disconnects, and size limits fail before installation",
    { skip: hostSupportsWindowsInstaller ? false : "Windows x64 required" },
    async (context) => {
        const parent = temporaryDirectory("download-errors");
        const base = {
            archive: realArtifact?.archive ?? Buffer.from("archive"),
            checksum: realArtifact?.checksum ?? "0".repeat(64) + "  archive\n",
        };
        const cases = [
            ["archive 404", { ...base, archiveStatus: 404 }, /HTTP status 404/],
            ["checksum 404", { ...base, checksumStatus: 404 }, /HTTP status 404/],
            ["empty archive", { ...base, archive: Buffer.alloc(0) }, /is empty/],
            ["connection closed", { ...base, disconnectArchive: true }, /download|request|response/i],
            [
                "large sidecar",
                { ...base, checksum: "a".repeat(4097) },
                /allowed size|exceeds/i,
            ],
            [
                "large archive header",
                {
                    ...base,
                    archiveHeaders: { "Content-Length": "268435457" },
                },
                /allowed size|exceeds/i,
            ],
        ];
        try {
            for (const [name, serverOptions, pattern] of cases) {
                await context.test(name, async () => {
                    const installDirectory = join(parent, name);
                    await withServer(serverOptions, async (server) => {
                        const result = await runPowerShellInstaller({
                            scriptUrl: `${server.baseUrl}/install.ps1`,
                            baseUrl: server.baseUrl,
                            installDirectory,
                        });
                        assertFailed(result, pattern);
                    });
                    assert.ok(!existsSync(installDirectory));
                });
            }
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "checksum format and mismatch failures never publish an installation",
    { skip: hostSupportsWindowsInstaller && realArtifact ? false : "Windows artifact required" },
    async (context) => {
        const parent = temporaryDirectory("checksum-errors");
        const cases = [
            ["short", "abcd  archive.zip\n", /invalid format/],
            ["nonhex", `${"z".repeat(64)}  archive.zip\n`, /invalid format/],
            ["mismatch", `${"0".repeat(64)}  archive.zip\n`, /verification failed/],
            ["multiple lines", `${realArtifact.checksum}extra\n`, /invalid format/],
        ];
        try {
            for (const [name, checksum, pattern] of cases) {
                await context.test(name, async () => {
                    const installDirectory = join(parent, name);
                    await withServer(
                        { archive: realArtifact.archive, checksum },
                        async (server) => {
                            const result = await runPowerShellInstaller({
                                scriptUrl: `${server.baseUrl}/install.ps1`,
                                baseUrl: server.baseUrl,
                                installDirectory,
                            });
                            assertFailed(result, pattern);
                        },
                    );
                    assert.ok(!existsSync(installDirectory));
                });
            }
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "unsafe ZIP inventories are rejected before extraction",
    { skip: hostSupportsWindowsInstaller && realArtifact ? false : "Windows artifact required" },
    async (context) => {
        const parent = temporaryDirectory("archive-security");
        const root = realArtifact.entries[0].name.replace(/\/$/, "");
        const unsafeCases = [
            [
                "traversal",
                [...cloneEntries(realArtifact.entries), { name: `${root}/../escape`, type: "file", data: Buffer.from("x") }],
                /traversal/,
            ],
            [
                "absolute",
                [...cloneEntries(realArtifact.entries), { name: "/escape", type: "file", data: Buffer.from("x") }],
                /absolute/,
            ],
            [
                "drive",
                [...cloneEntries(realArtifact.entries), { name: "C:/escape", type: "file", data: Buffer.from("x") }],
                /absolute/,
            ],
            [
                "backslash",
                [...cloneEntries(realArtifact.entries), { name: `${root}\\..\\escape`, type: "file", data: Buffer.from("x") }],
                /backslash/,
            ],
            [
                "multiple roots",
                [...cloneEntries(realArtifact.entries), { name: "other/file", type: "file", data: Buffer.from("x") }],
                /exactly one root/,
            ],
            [
                "duplicate",
                [...cloneEntries(realArtifact.entries), cloneEntries(realArtifact.entries)[1]],
                /duplicate/,
            ],
            [
                "symlink",
                [...cloneEntries(realArtifact.entries), { name: `${root}/link`, type: "symlink", linkName: "../escape" }],
                /symlink|reparse/,
            ],
        ];
        try {
            for (const [name, entries, pattern] of unsafeCases) {
                await context.test(name, async () => {
                    const installDirectory = join(parent, name);
                    const fixture = archiveFixture(entries);
                    await withServer(fixture, async (server) => {
                        const result = await runPowerShellInstaller({
                            scriptUrl: `${server.baseUrl}/install.ps1`,
                            baseUrl: server.baseUrl,
                            installDirectory,
                        });
                        assertFailed(result, pattern);
                    });
                    assert.ok(!existsSync(installDirectory));
                });
            }
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "missing files and invalid install manifests fail without a managed partial installation",
    { skip: hostSupportsWindowsInstaller && realArtifact ? false : "Windows artifact required" },
    async (context) => {
        const parent = temporaryDirectory("manifest-errors");
        const root = realArtifact.entries[0].name.replace(/\/$/, "");
        const manifestEntry = realArtifact.entries.find((entry) =>
            entry.name.endsWith("/install-manifest.json"),
        );
        const manifest = JSON.parse(manifestEntry.data.toString("utf8"));
        const variants = [
            ["missing LICENSE", removeEntry(realArtifact.entries, "/LICENSE"), /missing required entry/],
            [
                "invalid JSON",
                replaceEntry(realArtifact.entries, "/install-manifest.json", "{ invalid"),
                /not valid JSON/,
            ],
            [
                "wrong target",
                replaceEntry(
                    realArtifact.entries,
                    "/install-manifest.json",
                    JSON.stringify({ ...manifest, target: "linux-x64" }, null, 2) + "\n",
                ),
                /does not describe/,
            ],
            [
                "empty version",
                replaceEntry(
                    realArtifact.entries,
                    "/install-manifest.json",
                    JSON.stringify({ ...manifest, version: "" }, null, 2) + "\n",
                ),
                /invalid version/,
            ],
            [
                "missing entrypoint",
                replaceEntry(
                    realArtifact.entries,
                    "/install-manifest.json",
                    JSON.stringify(
                        Object.fromEntries(
                            Object.entries(manifest).filter(([key]) => key !== "entrypoint"),
                        ),
                        null,
                        2,
                    ) + "\n",
                ),
                /missing 'entrypoint'/,
            ],
            [
                "incomplete stdlib",
                removeEntry(realArtifact.entries, "/stdlib/aster/math.aster"),
                /incomplete standard library/,
            ],
            [
                "unexpected file",
                [
                    ...cloneEntries(realArtifact.entries),
                    {
                        name: `${root}/install.cmd`,
                        type: "file",
                        data: Buffer.from("unexpected"),
                    },
                ],
                /unexpected entry/,
            ],
        ];
        try {
            for (const [name, entries, pattern] of variants) {
                await context.test(name, async () => {
                    const installDirectory = join(parent, name);
                    await withServer(archiveFixture(entries), async (server) => {
                        const result = await runPowerShellInstaller({
                            scriptUrl: `${server.baseUrl}/install.ps1`,
                            baseUrl: server.baseUrl,
                            installDirectory,
                        });
                        assertFailed(result, pattern);
                    });
                    assert.ok(!existsSync(join(installDirectory, "install-state.json")));
                });
            }
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "directory protection preserves unmanaged, invalid-marker, and other-version content",
    { skip: hostSupportsWindowsInstaller && realArtifact ? false : "Windows artifact required" },
    async (context) => {
        const parent = temporaryDirectory("directory-protection");
        const cases = [
            [
                "unmanaged",
                (directory) => writeFileSync(join(directory, "keep.txt"), "keep"),
                /not managed/,
            ],
            [
                "invalid marker",
                (directory) => writeFileSync(join(directory, "install-state.json"), "{ invalid"),
                /not valid JSON/,
            ],
            [
                "other target",
                (directory) =>
                    writeFileSync(
                        join(directory, "install-state.json"),
                        JSON.stringify({
                            schema: 1,
                            product: "aster",
                            version: realArtifact.manifest.version,
                            target: "linux-x64",
                        }),
                    ),
                /invalid/,
            ],
        ];
        try {
            for (const [name, prepare, pattern] of cases) {
                await context.test(name, async () => {
                    const installDirectory = join(parent, name);
                    mkdirSync(installDirectory);
                    prepare(installDirectory);
                    const before = readdirSync(installDirectory);
                    await withServer(realArtifact, async (server) => {
                        const result = await runPowerShellInstaller({
                            scriptUrl: `${server.baseUrl}/install.ps1`,
                            baseUrl: server.baseUrl,
                            installDirectory,
                        });
                        assertFailed(result, pattern);
                    });
                    assert.deepEqual(readdirSync(installDirectory), before);
                });
            }
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "empty, traversal, root, control-character, and reparse install paths are rejected before download",
    { skip: hostSupportsWindowsInstaller && realArtifact ? false : "Windows artifact required" },
    async (context) => {
        const parent = temporaryDirectory("unsafe-install-paths");
        const outside = join(parent, "outside");
        const junction = join(parent, "junction");
        const nested = join(parent, "nested");
        mkdirSync(outside);
        mkdirSync(nested);
        symlinkSync(outside, junction, "junction");
        const cases = [
            ["traversal", `${nested}\\..\\escaped`, /must not contain unresolved/],
            ["root", parse(parent).root, /filesystem root/],
            ["control", join(parent, "bad\npath"), /control characters/],
            ["reparse ancestor", join(junction, "Aster"), /reparse point/],
        ];
        try {
            for (const [name, installDirectory, pattern] of cases) {
                await context.test(name, async () => {
                    await withServer(realArtifact, async (server) => {
                        const result = await runPowerShellInstaller({
                            scriptUrl: `${server.baseUrl}/install.ps1`,
                            baseUrl: server.baseUrl,
                            installDirectory: join(parent, "unused"),
                            extraEnvironment: { ASTER_INSTALL_DIR: installDirectory },
                        });
                        assertFailed(result, pattern);
                        assert.deepEqual(server.requests, ["/install.ps1"]);
                    });
                });
            }
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "unexpected managed-root content blocks both update and uninstall without touching the file",
    {
        skip:
            hostSupportsWindowsInstaller && realArtifact
                ? false
                : "Windows x64 M6A3 artifact is required",
    },
    async () => {
        const parent = temporaryDirectory("unexpected-managed-entry");
        const installDirectory = join(parent, "Aster");
        const unexpected = join(installDirectory, "my-project.aster");
        try {
            await withServer(realArtifact, async (server) => {
                const installed = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assert.equal(installed.status, 0, installed.stderr);
                writeFileSync(unexpected, "user content");

                const update = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assertFailed(update, /unexpected entry: my-project\.aster/);
                assert.deepEqual(server.requests.slice(-1), ["/install.ps1"]);

                const uninstall = await runPowerShellUninstaller({
                    scriptUrl: `${server.baseUrl}/uninstall.ps1`,
                    installDirectory,
                });
                assertFailed(uninstall, /unexpected entry: my-project\.aster/);
            });
            assert.equal(readFileSync(unexpected, "utf8"), "user content");
            assert.ok(existsSync(installDirectory));
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "nested managed reparse points block update and uninstall without touching their target",
    {
        skip:
            hostSupportsWindowsInstaller && realArtifact
                ? false
                : "Windows x64 M6A3 artifact is required",
    },
    async () => {
        const parent = temporaryDirectory("nested-reparse");
        const installDirectory = join(parent, "Aster");
        const outside = join(parent, "outside");
        mkdirSync(installDirectory);
        mkdirSync(join(installDirectory, "bin"));
        mkdirSync(outside);
        writeManagedState(installDirectory, realArtifact.manifest.version);
        writeFileSync(join(outside, "keep.txt"), "keep");
        symlinkSync(outside, join(installDirectory, "bin", "linked"), "junction");
        try {
            await withServer(realArtifact, async (server) => {
                const update = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assertFailed(update, /reparse point/);
                assert.deepEqual(server.requests, ["/install.ps1"]);
                const uninstall = await runPowerShellUninstaller({
                    scriptUrl: `${server.baseUrl}/uninstall.ps1`,
                    installDirectory,
                });
                assertFailed(uninstall, /reparse point/);
            });
            assert.equal(readFileSync(join(outside, "keep.txt"), "utf8"), "keep");
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "Windows uninstaller removes a managed installation and is idempotent",
    {
        skip:
            hostSupportsWindowsInstaller && realArtifact
                ? false
                : "Windows x64 M6A3 artifact is required",
    },
    async () => {
        const parent = temporaryDirectory("uninstall");
        const installDirectory = join(parent, "Aster");
        try {
            await withServer(realArtifact, async (server) => {
                const installed = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assert.equal(installed.status, 0, installed.stderr);
                const removed = await runPowerShellUninstaller({
                    scriptUrl: `${server.baseUrl}/uninstall.ps1`,
                    installDirectory,
                });
                assert.equal(removed.status, 0, removed.stderr);
                assert.match(removed.stdout, /ASTER uninstalled successfully/);
                assert.ok(!existsSync(installDirectory));

                const repeated = await runPowerShellUninstaller({
                    scriptUrl: `${server.baseUrl}/uninstall.ps1`,
                    installDirectory,
                });
                assert.equal(repeated.status, 0, repeated.stderr);
                assert.match(repeated.stdout, /ASTER is not installed/);
            });
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "Windows uninstaller rejects unmanaged, invalid, cross-target, traversal, and reparse destinations",
    { skip: hostSupportsWindowsInstaller ? false : "Windows x64 required" },
    async (context) => {
        const parent = temporaryDirectory("uninstall-safety");
        const cases = [
            [
                "unmanaged",
                (root) => writeFileSync(join(root, "keep.txt"), "keep"),
                /not managed/,
            ],
            [
                "invalid marker",
                (root) => writeFileSync(join(root, "install-state.json"), "{ invalid"),
                /not valid JSON/,
            ],
            [
                "other target",
                (root) => writeManagedState(root, "1.0.0", "linux-x64"),
                /another platform/,
            ],
        ];
        try {
            await withServer(
                {
                    archive: realArtifact?.archive ?? Buffer.from("archive"),
                    checksum: realArtifact?.checksum ?? "0".repeat(64) + "  archive\n",
                },
                async (server) => {
                    for (const [name, prepare, pattern] of cases) {
                        await context.test(name, async () => {
                            const root = join(parent, name);
                            mkdirSync(root);
                            prepare(root);
                            const before = snapshotTree(root);
                            const result = await runPowerShellUninstaller({
                                scriptUrl: `${server.baseUrl}/uninstall.ps1`,
                                installDirectory: root,
                            });
                            assertFailed(result, pattern);
                            assert.deepEqual(snapshotTree(root), before);
                        });
                    }

                    await context.test("traversal", async () => {
                        const nested = join(parent, "nested");
                        mkdirSync(nested);
                        const result = await runPowerShellUninstaller({
                            scriptUrl: `${server.baseUrl}/uninstall.ps1`,
                            installDirectory: `${nested}\\..\\escaped`,
                        });
                        assertFailed(result, /unsafe path/);
                    });

                    await context.test("reparse", async () => {
                        const target = join(parent, "reparse-target");
                        const link = join(parent, "reparse-link");
                        mkdirSync(target);
                        symlinkSync(target, link, "junction");
                        const result = await runPowerShellUninstaller({
                            scriptUrl: `${server.baseUrl}/uninstall.ps1`,
                            installDirectory: link,
                        });
                        assertFailed(result, /reparse point/);
                        assert.ok(existsSync(target));
                    });
                },
            );
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

test(
    "failed post-publication CLI validation removes staging and the new installation",
    { skip: hostSupportsWindowsInstaller && realArtifact ? false : "Windows artifact required" },
    async () => {
        const parent = temporaryDirectory("publication-failure");
        const installDirectory = join(parent, "Aster");
        const broken = replaceEntry(
            realArtifact.entries,
            "/bin/aster.exe",
            Buffer.from("not an executable"),
        );
        try {
            await withServer(archiveFixture(broken), async (server) => {
                const result = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assertFailed(result, /could not be started|validation/i);
            });
            assert.ok(!existsSync(installDirectory));
            assert.deepEqual(
                readdirSync(parent).filter((name) => name.startsWith(".aster-install-")),
                [],
            );
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);
