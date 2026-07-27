#!/usr/bin/env node

import { test } from "node:test";
import assert from "node:assert/strict";
import {
    existsSync,
    mkdtempSync,
    mkdirSync,
    readFileSync,
    rmSync,
    writeFileSync,
} from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { fileURLToPath } from "node:url";

import { sha256 } from "./package-release.mjs";

const repositoryRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const finalAssets = process.env.ASTER_RELEASE_ASSETS_DIR
    ? join(repositoryRoot, process.env.ASTER_RELEASE_ASSETS_DIR)
    : null;
const hostIsLinuxX64 = process.platform === "linux" && process.arch === "x64";

function loadLinuxRelease() {
    if (finalAssets) {
        const manifestPath = join(finalAssets, "release-manifest.json");
        if (!existsSync(manifestPath)) return null;
        const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
        const asset = manifest.assets.find((candidate) => candidate.target === "linux-x64");
        if (!asset) return null;
        return {
            version: manifest.version,
            archive: readFileSync(join(finalAssets, asset.alias)),
            install: readFileSync(join(finalAssets, "install.sh")),
            uninstall: readFileSync(join(finalAssets, "uninstall.sh")),
        };
    }
    const artifacts = join(repositoryRoot, "dist", "artifacts");
    const manifestPath = join(artifacts, "release-artifact.json");
    if (!existsSync(manifestPath)) return null;
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    if (manifest.target !== "linux-x64") return null;
    return {
        version: manifest.version,
        archive: readFileSync(join(artifacts, manifest.archive)),
        install: readFileSync(join(repositoryRoot, "install", "install.sh")),
        uninstall: readFileSync(join(repositoryRoot, "install", "uninstall.sh")),
    };
}

const linuxRelease = loadLinuxRelease();

function temporaryDirectory(label) {
    return mkdtempSync(join(tmpdir(), `aster-linux-installer-${label}-`));
}

async function startServer(release) {
    const requests = [];
    const checksum = `${sha256(release.archive)}  aster-linux-x64.tar.gz\n`;
    const server = createServer((request, response) => {
        requests.push(request.url);
        const resources = new Map([
            ["/install.sh", ["text/plain", release.install]],
            ["/uninstall.sh", ["text/plain", release.uninstall]],
            ["/aster-linux-x64.tar.gz", ["application/gzip", release.archive]],
            ["/aster-linux-x64.tar.gz.sha256", ["text/plain", checksum]],
        ]);
        const resource = resources.get(request.url);
        if (!resource) {
            response.writeHead(404);
            response.end("not found");
            return;
        }
        response.writeHead(200, { "Content-Type": resource[0] });
        response.end(resource[1]);
    });
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const address = server.address();
    return {
        baseUrl: `http://127.0.0.1:${address.port}`,
        requests,
        close: async () => {
            server.close();
            await once(server, "close");
        },
    };
}

function runPipedScript(url, environment) {
    return new Promise((resolve, reject) => {
        const child = spawn("sh", ["-c", 'curl -fsSL "$ASTER_TEST_SCRIPT_URL" | sh'], {
            cwd: tmpdir(),
            env: {
                ...process.env,
                ...environment,
                ASTER_TEST_SCRIPT_URL: url,
            },
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

function runCli(binary, commandArguments, currentDirectory, environment = {}) {
    return new Promise((resolve, reject) => {
        const child = spawn(binary, commandArguments, {
            cwd: currentDirectory,
            env: { ...process.env, ...environment },
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

function installerEnvironment(server, installDirectory) {
    return {
        ASTER_INSTALL_BASE_URL: server.baseUrl,
        ASTER_INSTALL_ALLOW_INSECURE: "1",
        ASTER_INSTALL_DIR: installDirectory,
        ASTER_INSTALL_SKIP_PATH: "1",
        ASTER_STDLIB: join(tmpdir(), "aster-test-missing-stdlib"),
    };
}

test(
    "Linux installer, repair, update, public CLI proof, and uninstall run through POSIX sh",
    {
        skip:
            hostIsLinuxX64 && linuxRelease
                ? false
                : "Linux x64 release artifact is required",
    },
    async () => {
        const root = temporaryDirectory("lifecycle ü");
        const installDirectory = join(root, "install with spaces");
        const proofParent = join(root, "proof");
        mkdirSync(proofParent);
        const server = await startServer(linuxRelease);
        const environment = installerEnvironment(server, installDirectory);
        try {
            const installed = await runPipedScript(`${server.baseUrl}/install.sh`, environment);
            assert.equal(installed.status, 0, installed.stderr);
            assert.match(installed.stdout, /ASTER installed successfully/);

            const binary = join(installDirectory, "bin", "aster");
            const cliEnvironment = {
                ...environment,
                PATH: `${join(installDirectory, "bin")}:${process.env.PATH ?? ""}`,
            };
            delete cliEnvironment.ASTER_STDLIB;
            for (const [commandArguments, cwd, pattern] of [
                [["--version"], root, new RegExp(linuxRelease.version.replaceAll(".", "\\."))],
                [["doctor"], root, /No problems found|completed with warnings/],
                [["new", "ReleaseProof"], proofParent, /ASTER project created/],
            ]) {
                const result = await runCli(binary, commandArguments, cwd, cliEnvironment);
                assert.equal(result.status, 0, result.stderr);
                assert.match(result.stdout, pattern);
            }
            const project = join(proofParent, "ReleaseProof");
            for (const command of ["check", "dump-hir", "dump-mir", "run"]) {
                const result = await runCli(binary, [command], project, cliEnvironment);
                assert.equal(result.status, 0, `${command}: ${result.stderr}`);
                assert.ok(result.stdout.length > 0);
            }

            const same = await runPipedScript(`${server.baseUrl}/install.sh`, environment);
            assert.equal(same.status, 0, same.stderr);
            assert.match(same.stdout, /already installed and healthy/);

            rmSync(join(installDirectory, "stdlib", "aster", "math.aster"));
            const repaired = await runPipedScript(`${server.baseUrl}/install.sh`, environment);
            assert.equal(repaired.status, 0, repaired.stderr);
            assert.match(repaired.stdout, /ASTER repaired successfully/);

            const statePath = join(installDirectory, "install-state.json");
            const state = JSON.parse(readFileSync(statePath, "utf8"));
            state.version = "0.0.0";
            writeFileSync(statePath, `${JSON.stringify(state, null, 2)}\n`);
            const updated = await runPipedScript(`${server.baseUrl}/install.sh`, environment);
            assert.equal(updated.status, 0, updated.stderr);
            assert.match(updated.stdout, /ASTER updated successfully/);
            assert.equal(
                JSON.parse(readFileSync(statePath, "utf8")).version,
                linuxRelease.version,
            );

            const removed = await runPipedScript(`${server.baseUrl}/uninstall.sh`, environment);
            assert.equal(removed.status, 0, removed.stderr);
            assert.match(removed.stdout, /ASTER uninstalled successfully/);
            assert.equal(existsSync(installDirectory), false);
            const repeated = await runPipedScript(`${server.baseUrl}/uninstall.sh`, environment);
            assert.equal(repeated.status, 0, repeated.stderr);
            assert.match(repeated.stdout, /ASTER is not installed/);
        } finally {
            await server.close();
            rmSync(root, { recursive: true, force: true });
        }
    },
);

test(
    "Linux rollback function restores the previous managed installation",
    { skip: hostIsLinuxX64 ? false : "Linux x64 required" },
    async () => {
        const root = temporaryDirectory("rollback");
        const installDirectory = join(root, "Aster");
        const backupDirectory = join(root, "Aster.backup.controlled");
        mkdirSync(installDirectory);
        mkdirSync(backupDirectory);
        writeFileSync(join(installDirectory, "new.txt"), "new");
        writeFileSync(join(backupDirectory, "old.txt"), "old");
        writeFileSync(
            join(backupDirectory, "install-state.json"),
            '{"schema":1,"product":"aster","version":"0.0.0","target":"linux-x64"}\n',
        );
        const source = linuxRelease
            ? linuxRelease.install.toString("utf8")
            : readFileSync(join(repositoryRoot, "install", "install.sh"), "utf8");
        const jsonFunction = source.slice(
            source.indexOf("json_string()"),
            source.indexOf("json_number()"),
        );
        const rollbackFunction = source.slice(
            source.indexOf("rollback_managed()"),
            source.indexOf("PREVIOUS_HEALTHY=0"),
        );
        const script = `set -eu
fail() { printf '%s\n' "error: $*" >&2; exit 1; }
${jsonFunction}
${rollbackFunction}
INSTALL_DIR='${installDirectory.replaceAll("'", "'\\''")}'
BACKUP_DIR='${backupDirectory.replaceAll("'", "'\\''")}'
INSTALLED_VERSION=0.0.0
PREVIOUS_HEALTHY=0
rollback_managed 'forced final validation failure'
`;
        try {
            const result = await new Promise((resolve, reject) => {
                const child = spawn("sh", ["-c", script]);
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
            assert.notEqual(result.status, 0);
            assert.match(result.stderr, /previous installation was restored/i);
            assert.equal(readFileSync(join(installDirectory, "old.txt"), "utf8"), "old");
            assert.equal(existsSync(join(installDirectory, "new.txt")), false);
            assert.equal(existsSync(backupDirectory), false);
        } finally {
            rmSync(root, { recursive: true, force: true });
        }
    },
);
