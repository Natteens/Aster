export default {
    branches: ["main"],
    tagFormat: "v${version}",
    plugins: [
        [
            "@semantic-release/commit-analyzer",
            {
                preset: "conventionalcommits",
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
        [
            "@semantic-release/github",
            {
                successCommentCondition: false,
                failCommentCondition: false,
                releasedLabels: false,
            },
        ],
    ],
};
