export default {
    branches: ["main"],
    repositoryUrl: "https://github.com/Natteens/Aster",
    tagFormat: "v${version}",
    plugins: [
        [
            "@semantic-release/commit-analyzer",
            {
                preset: "conventionalcommits",
                // ASTER is still 0.x: an explicitly marked incompatible change is
                // a minor release until the deliberate 1.0 transition removes
                // this override.
                releaseRules: [{ breaking: true, release: "minor" }],
            },
        ],
        [
            "@semantic-release/release-notes-generator",
            {
                preset: "conventionalcommits",
            },
        ],
        [
            "@semantic-release/changelog",
            {
                changelogFile: "CHANGELOG.md",
                changelogTitle: "# Changelog",
            },
        ],
        [
            "@semantic-release/exec",
            {
                prepareCmd: "node scripts/sync-release-version.mjs ${nextRelease.version}",
            },
        ],
        [
            "@semantic-release/git",
            {
                assets: [
                    "CHANGELOG.md",
                    "Cargo.toml",
                    "Cargo.lock",
                    "crates/*/Cargo.toml",
                    "editors/vscode/package.json",
                    "editors/vscode/package-lock.json",
                ],
                message: "chore(release): ${nextRelease.version} [skip ci]",
            },
        ],
    ],
};
