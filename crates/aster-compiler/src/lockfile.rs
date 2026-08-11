//! Deterministic root-project `Aster.lock` parsing and writing.
//!
//! The lockfile records only resolution results for Git sources. Package
//! graph edges and package semantics remain owned by each `Aster.toml`.

use std::{
    fmt::Write as _,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::manifest::{valid_identifier, validate_git_rev, validate_git_url};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lockfile {
    pub packages: Vec<LockedPackage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LockedPackage {
    pub name: String,
    pub git: String,
    pub rev: String,
    pub commit: String,
}

pub(crate) fn read(path: &Path) -> Result<Option<Lockfile>, String> {
    if path
        .parent()
        .is_some_and(|parent| parent.join(".Aster.lock.lock").exists())
    {
        return Err("another ASTER process is writing Aster.lock; retry the command".to_owned());
    }
    read_unlocked(path)
}

fn read_unlocked(path: &Path) -> Result<Option<Lockfile>, String> {
    match fs::read_to_string(path) {
        Ok(source) => parse(&source).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read Aster.lock: {error}")),
    }
}

pub(crate) fn parse(source: &str) -> Result<Lockfile, String> {
    let document = source
        .parse::<toml::Value>()
        .map_err(|error| format!("invalid Aster.lock: {error}"))?;
    let table = document
        .as_table()
        .ok_or_else(|| "Aster.lock must contain a TOML table".to_owned())?;
    if let Some(field) = table.keys().find(|field| field.as_str() != "package") {
        return Err(format!("unknown Aster.lock field `{field}`"));
    }
    let packages = table
        .get("package")
        .map_or(Ok(Vec::new()), parse_packages)?;
    let mut lockfile = Lockfile { packages };
    lockfile
        .packages
        .sort_by(|left, right| left.name.cmp(&right.name));
    for pair in lockfile.packages.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(format!(
                "Aster.lock contains duplicate package `{}`",
                pair[0].name
            ));
        }
    }
    Ok(lockfile)
}

fn parse_packages(value: &toml::Value) -> Result<Vec<LockedPackage>, String> {
    let array = value
        .as_array()
        .ok_or_else(|| "Aster.lock `package` must be an array of tables".to_owned())?;
    array
        .iter()
        .enumerate()
        .map(|(index, value)| parse_package(index, value))
        .collect()
}

fn parse_package(index: usize, value: &toml::Value) -> Result<LockedPackage, String> {
    let table = value
        .as_table()
        .ok_or_else(|| format!("Aster.lock package {} must be a table", index + 1))?;
    if let Some(field) = table
        .keys()
        .find(|field| !matches!(field.as_str(), "name" | "git" | "rev" | "commit"))
    {
        return Err(format!(
            "unknown field `{field}` in Aster.lock package {}",
            index + 1
        ));
    }
    let field = |name: &str| -> Result<String, String> {
        table
            .get(name)
            .and_then(toml::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                format!(
                    "Aster.lock package {} requires a non-empty `{name}` string",
                    index + 1
                )
            })
    };
    let package = LockedPackage {
        name: field("name")?,
        git: field("git")?,
        rev: field("rev")?,
        commit: field("commit")?.to_ascii_lowercase(),
    };
    if !valid_identifier(&package.name) {
        return Err(format!(
            "Aster.lock package name `{}` is not a valid ASTER identifier",
            package.name
        ));
    }
    validate_git_url(&package.git)
        .map_err(|message| format!("Aster.lock package `{}` {message}", package.name))?;
    validate_git_rev(&package.rev)
        .map_err(|message| format!("Aster.lock package `{}` {message}", package.name))?;
    if !valid_commit(&package.commit) {
        return Err(format!(
            "Aster.lock package `{}` commit must be a full hexadecimal commit SHA",
            package.name
        ));
    }
    if valid_commit(&package.rev) && !package.rev.eq_ignore_ascii_case(&package.commit) {
        return Err(format!(
            "Aster.lock package `{}` commit does not match its immutable revision",
            package.name
        ));
    }
    Ok(package)
}

pub(crate) fn render(lockfile: &Lockfile) -> String {
    let mut packages = lockfile.packages.clone();
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    let mut output = String::new();
    for (index, package) in packages.iter().enumerate() {
        if index != 0 {
            output.push('\n');
        }
        output.push_str("[[package]]\n");
        writeln!(output, "name = {:?}", package.name).expect("writing to a String cannot fail");
        writeln!(output, "git = {:?}", package.git).expect("writing to a String cannot fail");
        writeln!(output, "rev = {:?}", package.rev).expect("writing to a String cannot fail");
        writeln!(output, "commit = {:?}", package.commit).expect("writing to a String cannot fail");
    }
    output
}

pub(crate) fn write_atomic(
    path: &Path,
    lockfile: &Lockfile,
    expected: Option<&Lockfile>,
) -> Result<bool, String> {
    let contents = render(lockfile);
    let parent = path
        .parent()
        .ok_or_else(|| "Aster.lock has no parent directory".to_owned())?;
    let lock_path = parent.join(".Aster.lock.lock");
    let lock = LockfileWriteLock::acquire(&lock_path)?;
    let current = read_unlocked(path)?;
    if current.as_ref() != expected {
        return Err(
            "Aster.lock changed while dependencies were resolving; retry `aster fetch`".to_owned(),
        );
    }
    if fs::read_to_string(path).ok().as_deref() == Some(contents.as_str()) {
        return Ok(false);
    }
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temporary: PathBuf = parent.join(format!(".Aster.lock.tmp-{}-{id}", std::process::id()));
    let backup: PathBuf = parent.join(format!(".Aster.lock.backup-{}-{id}", std::process::id()));
    let result = (|| -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("could not create temporary Aster.lock: {error}"))?;
        file.write_all(contents.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("could not write temporary Aster.lock: {error}"))?;
        let had_previous = path.exists();
        if had_previous {
            fs::rename(path, &backup)
                .map_err(|error| format!("could not stage the previous Aster.lock: {error}"))?;
        }
        if let Err(error) = fs::rename(&temporary, path) {
            if had_previous && let Err(rollback) = fs::rename(&backup, path) {
                return Err(format!(
                    "could not publish Aster.lock: {error}; restoring the previous lockfile also failed: {rollback}"
                ));
            }
            return Err(format!("could not publish Aster.lock: {error}"));
        }
        if had_previous {
            let _ = fs::remove_file(&backup);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    drop(lock);
    result.map(|()| true)
}

pub(crate) fn remove_atomic(path: &Path, expected: Option<&Lockfile>) -> Result<bool, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Aster.lock has no parent directory".to_owned())?;
    let lock_path = parent.join(".Aster.lock.lock");
    let lock = LockfileWriteLock::acquire(&lock_path)?;
    let current = read_unlocked(path)?;
    if current.as_ref() != expected {
        return Err(
            "Aster.lock changed while dependencies were resolving; retry `aster fetch`".to_owned(),
        );
    }
    let changed = match fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("could not remove obsolete Aster.lock: {error}")),
    };
    drop(lock);
    Ok(changed)
}

struct LockfileWriteLock {
    path: PathBuf,
}

impl LockfileWriteLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    "another ASTER process is writing Aster.lock; retry `aster fetch`".to_owned()
                } else {
                    format!("could not lock Aster.lock: {error}")
                }
            })?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for LockfileWriteLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn valid_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{LockedPackage, Lockfile, parse, render, write_atomic};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn sample_lockfile() -> Lockfile {
        Lockfile {
            packages: vec![LockedPackage {
                name: "math".to_owned(),
                git: "https://example.invalid/math.git".to_owned(),
                rev: "main".to_owned(),
                commit: "a".repeat(40),
            }],
        }
    }

    #[test]
    fn lockfiles_are_sorted_and_rendered_deterministically() {
        let lockfile = Lockfile {
            packages: vec![
                LockedPackage {
                    name: "zeta".to_owned(),
                    git: "https://example.invalid/zeta.git".to_owned(),
                    rev: "main".to_owned(),
                    commit: "a".repeat(40),
                },
                LockedPackage {
                    name: "alpha".to_owned(),
                    git: "https://example.invalid/alpha.git".to_owned(),
                    rev: "v1".to_owned(),
                    commit: "b".repeat(40),
                },
            ],
        };
        let first = render(&lockfile);
        let second = render(&parse(&first).expect("parse rendered lockfile"));
        assert_eq!(first, second);
        assert!(first.ends_with('\n'));
        assert!(first.find("alpha").unwrap() < first.find("zeta").unwrap());
        assert!(!first.contains("version"));
    }

    #[test]
    fn lockfiles_reject_unknown_fields_and_short_commits() {
        for source in [
            "version = 1\n",
            "[[package]]\nname = \"math\"\ngit = \"https://example.invalid/math.git\"\nrev = \"main\"\ncommit = \"abc123\"\n",
            "[[package]]\nname = \"math\"\ngit = \"https://example.invalid/math.git\"\nrev = \"main\"\ncommit = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nextra = 1\n",
            "[[package]]\nname = \"math\"\ngit = \"https://example.invalid/math.git\"\nrev = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\ncommit = \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\n",
        ] {
            assert!(parse(source).is_err());
        }
    }

    #[test]
    fn atomic_writes_are_idempotent_and_leave_no_staging_files() {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("aster-lockfile-{}-{id}", std::process::id()));
        fs::create_dir_all(&directory).expect("create lockfile test directory");
        let path = directory.join("Aster.lock");
        let lockfile = sample_lockfile();
        assert!(write_atomic(&path, &lockfile, None).expect("first lockfile write"));
        assert!(!write_atomic(&path, &lockfile, Some(&lockfile)).expect("repeated lockfile write"));
        assert_eq!(
            fs::read_to_string(&path).expect("read lockfile"),
            render(&lockfile)
        );
        assert_eq!(fs::read_dir(&directory).expect("list directory").count(), 1);
        fs::remove_dir_all(directory).expect("remove lockfile test directory");
    }

    #[test]
    fn atomic_writes_reject_a_concurrent_lockfile_change() {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("aster-lockfile-race-{}-{id}", std::process::id()));
        fs::create_dir_all(&directory).expect("create lockfile test directory");
        let path = directory.join("Aster.lock");
        let previous = sample_lockfile();
        let mut current = previous.clone();
        current.packages[0].commit = "b".repeat(40);
        fs::write(&path, render(&current)).expect("write concurrent lockfile state");

        let mut replacement = previous.clone();
        replacement.packages[0].commit = "c".repeat(40);
        let error = write_atomic(&path, &replacement, Some(&previous))
            .expect_err("concurrent state must not be overwritten");
        assert!(error.contains("changed while dependencies were resolving"));
        assert_eq!(
            fs::read_to_string(&path).expect("read lockfile"),
            render(&current)
        );

        fs::remove_dir_all(directory).expect("remove lockfile test directory");
    }
}
