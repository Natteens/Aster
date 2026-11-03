import { spawnSync } from "node:child_process";

/** Run git in `repositoryRoot` and return trimmed stdout; throws unless it succeeds with output. */
export function git(repositoryRoot, arguments_, { allowEmpty = false } = {}) {
    const result = spawnSync("git", arguments_, {
        cwd: repositoryRoot,
        encoding: "utf8",
    });
    if (result.error) throw new Error(`git ${arguments_[0]} failed: ${result.error.message}`);
    if (result.status !== 0) {
        throw new Error(`git ${arguments_[0]} failed`);
    }
    const output = result.stdout.trim();
    if (!allowEmpty && output.length === 0) {
        throw new Error(`git ${arguments_[0]} returned no result`);
    }
    return output;
}

/** The object id of exactly one remote ref on `origin`; throws when missing or ambiguous. */
export function remoteRef(repositoryRoot, ref) {
    const output = git(repositoryRoot, ["ls-remote", "--refs", "origin", ref]);
    const lines = output.split(/\r?\n/).filter(Boolean);
    if (lines.length !== 1) throw new Error(`remote ref ${ref} is missing or ambiguous`);
    const [sha, name] = lines[0].split(/\s+/);
    if (name !== ref) throw new Error(`remote ref ${ref} returned an unexpected name`);
    return sha;
}
