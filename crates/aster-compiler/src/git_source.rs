//! Safe materialization and validation of immutable Git package sources.

use std::{
    env,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(debug_assertions)]
use std::path::Component;

use crate::lockfile::valid_commit;

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn cache_root() -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    if let Some(value) = env::var_os("ASTER_GIT_CACHE_DIR") {
        let path = PathBuf::from(value);
        if path.as_os_str().is_empty() {
            return Err("ASTER_GIT_CACHE_DIR is empty".to_owned());
        }
        return Ok(path);
    }

    #[cfg(windows)]
    {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.join("Aster").join("cache").join("git"))
            .ok_or_else(|| {
                "LOCALAPPDATA is unavailable; cannot locate the ASTER Git cache".to_owned()
            })
    }
    #[cfg(not(windows))]
    {
        if let Some(path) = env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(path).join("aster").join("git"));
        }
        env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.join(".cache").join("aster").join("git"))
            .ok_or_else(|| "HOME is unavailable; cannot locate the ASTER Git cache".to_owned())
    }
}

pub(crate) fn cache_key(git: &str, commit: &str) -> String {
    let mut input = Vec::with_capacity(git.len() + commit.len() + 1);
    input.extend_from_slice(git.as_bytes());
    input.push(0);
    input.extend_from_slice(commit.as_bytes());
    hex(&sha256(&input))
}

// Small self-contained SHA-256 used only for deterministic cache addressing.
// This is not an authenticity mechanism; Git commits and the checkout itself
// are validated separately.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let bit_length = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());
    let mut hash = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(bytes.try_into().expect("four-byte SHA word"));
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = hash;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let first = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        for (state, value) in hash.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *state = state.wrapping_add(value);
        }
    }
    let mut output = [0_u8; 32];
    for (bytes, value) in output.chunks_exact_mut(4).zip(hash) {
        bytes.copy_from_slice(&value.to_be_bytes());
    }
    output
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        },
    )
}

pub(crate) fn cached_source(git: &str, commit: &str) -> Result<PathBuf, String> {
    let root = cache_root()?;
    let path = root.join(cache_key(git, commit));
    validate_checkout(&path, commit).map_err(|reason| {
        format!(
            "Git package cache for `{git}` at `{commit}` is unavailable or corrupt: {reason}; run `aster fetch`"
        )
    })?;
    fs::canonicalize(&path).map_err(|error| format!("could not resolve Git package cache: {error}"))
}

pub(crate) fn resolve_revision(git: &str, rev: &str) -> Result<String, String> {
    if valid_commit(rev) {
        return Ok(rev.to_ascii_lowercase());
    }
    let remote = remote_argument(git)?;
    let output = run_git(
        None,
        remote.allow_file,
        ["ls-remote", "--heads", "--tags", remote.value.as_str()],
    )?;
    let branch = format!("refs/heads/{rev}");
    let tag = format!("refs/tags/{rev}");
    let peeled = format!("{tag}^{{}}");
    let mut branch_commit = None;
    let mut tag_commit = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((commit, reference)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if reference == branch {
            branch_commit = Some(commit.to_ascii_lowercase());
        } else if reference == peeled || (reference == tag && tag_commit.is_none()) {
            tag_commit = Some(commit.to_ascii_lowercase());
        }
    }
    match (branch_commit, tag_commit) {
        (Some(_), Some(_)) => Err(format!(
            "Git revision `{rev}` is ambiguous because both a branch and tag have that name"
        )),
        (Some(commit), None) | (None, Some(commit)) if valid_commit(&commit) => Ok(commit),
        _ => Err(format!("Git revision `{rev}` was not found at `{git}`")),
    }
}

pub(crate) fn materialize(git: &str, commit: &str) -> Result<PathBuf, String> {
    if !valid_commit(commit) {
        return Err("refusing to materialize a non-full Git commit SHA".to_owned());
    }
    let cache = cache_root()?;
    fs::create_dir_all(&cache)
        .map_err(|error| format!("could not create the ASTER Git cache: {error}"))?;
    let key = cache_key(git, commit);
    let destination = cache.join(&key);
    if validate_checkout(&destination, commit).is_ok() {
        return fs::canonicalize(destination)
            .map_err(|error| format!("could not resolve Git package cache: {error}"));
    }

    let lock_path = cache.join(format!(".{key}.lock"));
    let lock = CacheLock::acquire(&lock_path)?;
    if validate_checkout(&destination, commit).is_ok() {
        drop(lock);
        return fs::canonicalize(destination)
            .map_err(|error| format!("could not resolve Git package cache: {error}"));
    }

    let id = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
    let staging = cache.join(format!(".{key}.staging-{}-{id}", std::process::id()));
    let remote = remote_argument(git)?;
    let result = (|| -> Result<(), String> {
        fs::create_dir(&staging)
            .map_err(|error| format!("could not create Git package staging: {error}"))?;
        run_git(Some(&staging), remote.allow_file, ["init", "--quiet"])?;
        run_git(
            Some(&staging),
            remote.allow_file,
            ["remote", "add", "origin", remote.value.as_str()],
        )?;
        run_git(
            Some(&staging),
            remote.allow_file,
            [
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                "--depth=1",
                "origin",
                commit,
            ],
        )?;
        run_git(
            Some(&staging),
            remote.allow_file,
            ["checkout", "--quiet", "--detach", commit],
        )?;
        validate_checkout(&staging, commit)?;
        if destination.exists() {
            if is_link_or_reparse(&destination)? {
                return Err("refusing to replace a linked Git cache entry".to_owned());
            }
            fs::remove_dir_all(&destination)
                .map_err(|error| format!("could not replace corrupt Git package cache: {error}"))?;
        }
        fs::rename(&staging, &destination)
            .map_err(|error| format!("could not publish Git package cache: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    drop(lock);
    result?;
    fs::canonicalize(destination)
        .map_err(|error| format!("could not resolve Git package cache: {error}"))
}

fn validate_checkout(path: &Path, commit: &str) -> Result<(), String> {
    if is_link_or_reparse(path)? {
        return Err("cache entry is a symbolic link or reparse point".to_owned());
    }
    if !path.is_dir() || !path.join("Aster.toml").is_file() || !path.join(".git").is_dir() {
        return Err("required checkout files are missing".to_owned());
    }
    reject_unsafe_entries(path, path)?;
    if path.join(".gitmodules").exists() {
        return Err("submodules are not supported".to_owned());
    }
    let index = git_text(path, false, ["ls-files", "--stage"])?;
    if index.lines().any(|line| line.starts_with("160000 ")) {
        return Err("submodules are not supported".to_owned());
    }
    let index_flags = git_text(path, false, ["ls-files", "-v"])?;
    if index_flags.lines().any(|line| !line.starts_with("H ")) {
        return Err("checkout index contains unsupported hidden or sparse entries".to_owned());
    }
    let head = git_text(path, false, ["rev-parse", "HEAD"])?;
    if !head.eq_ignore_ascii_case(commit) {
        return Err(format!("checkout HEAD is `{head}`, expected `{commit}`"));
    }
    let status = git_text(
        path,
        false,
        [
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?;
    if !status.is_empty() {
        return Err("checkout contains unexpected modifications".to_owned());
    }
    Ok(())
}

fn reject_unsafe_entries(root: &Path, directory: &Path) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("could not inspect Git checkout: {error}"))?
    {
        let entry = entry.map_err(|error| format!("could not inspect Git checkout: {error}"))?;
        if entry.file_name() == ".git" && directory == root {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("could not inspect Git checkout entry: {error}"))?;
        if metadata_is_link_or_reparse(&metadata) {
            return Err(format!(
                "Git checkout contains unsupported symbolic link `{}`",
                entry.path().display()
            ));
        }
        if metadata.is_dir() {
            reject_unsafe_entries(root, &entry.path())?;
        } else if !metadata.is_file() {
            return Err(format!(
                "Git checkout contains unsupported entry `{}`",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

struct RemoteArgument {
    value: String,
    allow_file: bool,
}

fn remote_argument(git: &str) -> Result<RemoteArgument, String> {
    #[cfg(debug_assertions)]
    if let Some(root) = env::var_os("ASTER_GIT_TEST_REMOTE_ROOT") {
        if let Some(relative) = git.strip_prefix("https://example.invalid/") {
            let relative = Path::new(relative);
            if relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err("invalid test Git repository path".to_owned());
            }
            let root = fs::canonicalize(root)
                .map_err(|error| format!("invalid test Git remote root: {error}"))?;
            let path = fs::canonicalize(root.join(relative))
                .map_err(|error| format!("test Git repository is unavailable: {error}"))?;
            if !path.starts_with(&root) {
                return Err("test Git repository escapes its root".to_owned());
            }
            let path = path.to_string_lossy().replace('\\', "/");
            let path = path.strip_prefix("//?/").unwrap_or(&path);
            let value = if path.starts_with('/') {
                format!("file://{path}")
            } else {
                format!("file:///{path}")
            };
            return Ok(RemoteArgument {
                value,
                allow_file: true,
            });
        }
    }
    Ok(RemoteArgument {
        value: git.to_owned(),
        allow_file: false,
    })
}

fn git_text<const N: usize>(
    directory: &Path,
    allow_file: bool,
    arguments: [&str; N],
) -> Result<String, String> {
    let output = run_git(Some(directory), allow_file, arguments)?;
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| "Git returned non-UTF-8 output".to_owned())
}

fn run_git<const N: usize>(
    directory: Option<&Path>,
    allow_file: bool,
    arguments: [&str; N],
) -> Result<Output, String> {
    let mut command = git_command(allow_file);
    if let Some(directory) = directory {
        command.arg("-C").arg(directory);
    }
    let output = command
        .args(arguments)
        .output()
        .map_err(|error| format!("could not execute Git: {error}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message
            .lines()
            .next()
            .unwrap_or("Git command failed")
            .trim();
        Err(format!("Git command failed: {message}"))
    }
}

fn git_command(allow_file: bool) -> Command {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut command = Command::new("git");
    command
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_EXEC_PATH")
        .env_remove("GIT_TEMPLATE_DIR")
        .env_remove("GIT_PROXY_COMMAND")
        .env_remove("GIT_SSH")
        .env_remove("GIT_SSH_COMMAND")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .arg("-c")
        .arg(format!("core.hooksPath={null}"))
        .args(["-c", "credential.helper="])
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "gc.auto=0"])
        .args(["-c", "maintenance.auto=false"])
        .args(["-c", "submodule.recurse=false"])
        .args(["-c", "fetch.recurseSubmodules=false"])
        .args(["-c", "http.followRedirects=false"])
        .args(["-c", "protocol.allow=never"])
        .args(["-c", "protocol.https.allow=always"])
        .args(["-c", "protocol.ext.allow=never"])
        .args([
            "-c",
            if allow_file {
                "protocol.file.allow=always"
            } else {
                "protocol.file.allow=never"
            },
        ]);
    command
}

fn is_link_or_reparse(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata_is_link_or_reparse(&metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("could not inspect Git cache entry: {error}")),
    }
}

fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

struct CacheLock {
    path: PathBuf,
}

impl CacheLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        fs::create_dir(path).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                "another ASTER process is preparing this Git package; retry `aster fetch`"
                    .to_owned()
            } else {
                format!("could not lock the Git package cache: {error}")
            }
        })?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{cache_key, git_command};

    #[test]
    fn cache_keys_include_the_exact_url_and_commit() {
        let first = cache_key(
            "https://example.invalid/math.git",
            "0123456789abcdef0123456789abcdef01234567",
        );
        let second = cache_key(
            "https://example.invalid/math.git",
            "1123456789abcdef0123456789abcdef01234567",
        );
        assert_eq!(first.len(), 64);
        assert_eq!(
            first,
            "46537ede031c02b9b77de6710ac5d445be0f3fb07a87d31fc70045222f7f18a1"
        );
        assert_ne!(first, second);
        assert_eq!(
            super::hex(&super::sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn git_subprocess_configuration_disables_ambient_execution_paths() {
        let command = git_command(false);
        assert_eq!(command.get_program(), "git");
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let configurations = arguments
            .windows(2)
            .filter_map(|pair| (pair[0] == "-c").then_some(pair[1].as_str()))
            .collect::<Vec<_>>();
        for required in [
            "credential.helper=",
            "core.fsmonitor=false",
            "fetch.recurseSubmodules=false",
            "http.followRedirects=false",
            "protocol.allow=never",
            "protocol.https.allow=always",
            "protocol.ext.allow=never",
            "protocol.file.allow=never",
        ] {
            assert!(configurations.contains(&required), "missing {required}");
        }
    }
}
