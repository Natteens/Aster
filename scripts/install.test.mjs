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
import { dirname, join, parse } from "node:path";
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
const powerShellExecutable =
    "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
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
    const server = createServer((request, response) => {
        requests.push(request.url);
        if (request.url === "/install.ps1") {
            response.writeHead(200, { "Content-Type": "text/plain" });
            response.end(installerScript);
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
    assert.match(windows, /aster-windows-x64\.zip/);
    assert.match(linux, /aster-linux-x64\.tar\.gz/);
    for (const source of [windows, linux]) {
        assert.match(source, /ASTER_INSTALL_BASE_URL/);
        assert.match(source, /ASTER_INSTALL_ALLOW_INSECURE/);
        assert.match(source, /ASTER_INSTALL_DIR/);
        assert.match(source, /ASTER_INSTALL_SKIP_PATH/);
        assert.ok(!source.includes("0.41.0"), "installer must not hardcode a release version");
        assert.ok(!/\beval\b/.test(source), "installer must not use eval");
    }
    assert.ok(!windows.includes("$PSScriptRoot"));
    assert.ok(!windows.toLowerCase().includes("setx"));
    assert.match(linux, /^#!\/bin\/sh\nset -eu/m);
    assert.doesNotMatch(linux, /^[ \t]*\[\[/m);
    assert.ok(!linux.includes("pipefail"));
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
    "same managed version is validated without replacement",
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
                const before = statSync(
                    join(installDirectory, "bin", "aster.exe"),
                ).size;
                const second = await runPowerShellInstaller({
                    scriptUrl: `${server.baseUrl}/install.ps1`,
                    baseUrl: server.baseUrl,
                    installDirectory,
                });
                assert.equal(second.status, 0, second.stderr);
                assert.match(second.stdout, /already installed and valid/);
                assert.equal(
                    statSync(join(installDirectory, "bin", "aster.exe")).size,
                    before,
                );
            });
        } finally {
            rmSync(parent, { recursive: true, force: true });
        }
    },
);

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
                "other version",
                (directory) =>
                    writeFileSync(
                        join(directory, "install-state.json"),
                        JSON.stringify({
                            schema: 1,
                            product: "aster",
                            version: "0.0.0",
                            target: "windows-x64",
                        }),
                    ),
                /another version/,
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
