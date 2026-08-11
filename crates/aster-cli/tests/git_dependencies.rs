//! Public CLI contract for Git dependencies and `Aster.lock`.
//!
//! Every remote is a disposable local Git repository reached through the
//! compiler's debug-only HTTPS mapping. No test contacts the network.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    remotes: PathBuf,
    cache: PathBuf,
    app: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("aster-git-{label}-{}-{id}", std::process::id()));
        let remotes = root.join("remotes");
        let cache = root.join("cache");
        let app = root.join("app");
        fs::create_dir_all(&remotes).expect("create remote root");
        fs::create_dir_all(app.join("app")).expect("create app");
        Self {
            root,
            remotes,
            cache,
            app,
        }
    }

    fn repository(&self, name: &str, manifest: &str, source_path: &str, source: &str) -> PathBuf {
        let repository = self.remotes.join(format!("{name}.git"));
        fs::create_dir_all(&repository).expect("create repository");
        git(&repository, ["init", "--quiet"]);
        git(&repository, ["config", "user.name", "ASTER Test"]);
        git(
            &repository,
            ["config", "user.email", "aster-test@example.invalid"],
        );
        git(&repository, ["checkout", "--quiet", "-b", "main"]);
        write(&repository.join("Aster.toml"), manifest);
        write(&repository.join(source_path), source);
        git(&repository, ["add", "."]);
        git(&repository, ["commit", "--quiet", "-m", "initial"]);
        repository
    }

    fn write_app(&self, rev: &str) -> PathBuf {
        write(
            &self.app.join("Aster.toml"),
            &format!(
                "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n\n[dependencies]\nmath = {{ git = \"https://example.invalid/math.git\", rev = \"{rev}\" }}\n"
            ),
        );
        let root = self.app.join("app/main.aster");
        write(
            &root,
            "namespace app; using math; public class Program { public static int Main() { return Answer(); } }",
        );
        root
    }

    fn aster(&self, arguments: &[&str]) -> Output {
        self.aster_in(&self.app, arguments)
    }

    fn aster_in(&self, directory: &Path, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aster"))
            .args(arguments)
            .current_dir(directory)
            .env("ASTER_GIT_TEST_REMOTE_ROOT", &self.remotes)
            .env("ASTER_GIT_CACHE_DIR", &self.cache)
            .output()
            .expect("run ASTER CLI")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn fetch_locks_transitives_and_builds_offline_until_an_explicit_update() {
    let fixture = Fixture::new("lifecycle");
    fixture.repository(
        "numbers",
        "[package]\nname = \"numbers\"\n",
        "numbers/value.aster",
        "namespace numbers; public int Base() { return 40; }",
    );
    let math = fixture.repository(
        "math",
        "[package]\nname = \"math\"\n\n[dependencies]\nnumbers = { git = \"https://example.invalid/numbers.git\", rev = \"main\" }\n",
        "math/answer.aster",
        "namespace math; using numbers; public int Answer() { return Base() + 2; }",
    );
    write(
        &math.join("Aster.lock"),
        "this dependency lockfile is ignored\n",
    );
    git(&math, ["add", "Aster.lock"]);
    git(
        &math,
        ["commit", "--quiet", "-m", "dependency lock ignored"],
    );
    let root = fixture.write_app("main");

    let missing = fixture.aster(&["check"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("run `aster fetch`"));

    let fetched = fixture.aster(&["fetch"]);
    assert!(fetched.status.success(), "{}", stderr(&fetched));
    let lock_path = fixture.app.join("Aster.lock");
    let first_lock = fs::read_to_string(&lock_path).expect("read lockfile");
    assert_eq!(first_lock.matches("[[package]]").count(), 2);
    assert!(
        first_lock.find("name = \"math\"").unwrap()
            < first_lock.find("name = \"numbers\"").unwrap()
    );
    assert!(!first_lock.contains("version ="));
    let repeated = fixture.aster(&["fetch"]);
    assert!(repeated.status.success(), "{}", stderr(&repeated));
    assert_eq!(
        fs::read_to_string(&lock_path).expect("read repeated lockfile"),
        first_lock
    );

    let run = fixture.aster(&["run"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run).trim(), "42");
    let nested_run = fixture.aster_in(&fixture.app.join("app"), &["run"]);
    assert!(nested_run.status.success(), "{}", stderr(&nested_run));
    assert_eq!(stdout(&nested_run).trim(), "42");

    write(
        &math.join("math/answer.aster"),
        "namespace math; using numbers; public int Answer() { return Base() + 3; }",
    );
    git(&math, ["add", "."]);
    git(&math, ["commit", "--quiet", "-m", "move branch"]);
    let moved_commit = git_text(&math, ["rev-parse", "HEAD"]);

    let still_locked = fixture.aster(&["run"]);
    assert!(still_locked.status.success(), "{}", stderr(&still_locked));
    assert_eq!(stdout(&still_locked).trim(), "42");

    let numbers_before = locked_commit(&first_lock, "numbers");
    let updated = fixture.aster(&["fetch", "--update", "math"]);
    assert!(updated.status.success(), "{}", stderr(&updated));
    let second_lock = fs::read_to_string(&lock_path).expect("read updated lockfile");
    assert!(second_lock.contains(&moved_commit));
    assert_eq!(locked_commit(&second_lock, "numbers"), numbers_before);
    let rerun = fixture.aster(&["run"]);
    assert!(rerun.status.success(), "{}", stderr(&rerun));
    assert_eq!(stdout(&rerun).trim(), "43");

    fs::rename(&fixture.remotes, fixture.root.join("remotes-offline"))
        .expect("make remotes unavailable");
    let failed_update = fixture.aster(&["fetch", "--update", "math"]);
    assert_eq!(failed_update.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(&lock_path).expect("lock survives failed update"),
        second_lock
    );
    let offline_fetch = fixture.aster(&["fetch"]);
    assert!(offline_fetch.status.success(), "{}", stderr(&offline_fetch));
    for command in [["check"].as_slice(), ["run"].as_slice()] {
        let offline = fixture.aster(command);
        assert!(offline.status.success(), "{}", stderr(&offline));
    }
    write(&root, "namespace app; public class Program {");
    let watch = fixture.aster(&["watch", root.to_str().expect("UTF-8 path")]);
    assert_eq!(watch.status.code(), Some(1));
    assert!(!stderr(&watch).contains("Git command"));
    fs::remove_dir_all(&fixture.cache).expect("remove cache while remote is unavailable");
    let unavailable = fixture.aster(&["fetch"]);
    assert_eq!(unavailable.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(&lock_path).expect("lock survives failed fetch"),
        second_lock
    );
}

#[test]
fn targeted_update_rewrites_only_the_changed_reachable_graph() {
    let fixture = Fixture::new("update-graph");
    fixture.repository(
        "numbers",
        "[package]\nname = \"numbers\"\n",
        "numbers/value.aster",
        "namespace numbers; public int Base() { return 40; }",
    );
    fixture.repository(
        "replacement",
        "[package]\nname = \"replacement\"\n",
        "replacement/value.aster",
        "namespace replacement; public int Base() { return 41; }",
    );
    fixture.repository(
        "unrelated",
        "[package]\nname = \"unrelated\"\n",
        "unrelated/value.aster",
        "namespace unrelated; public int Unused() { return 7; }",
    );
    let math = fixture.repository(
        "math",
        "[package]\nname = \"math\"\n\n[dependencies]\nnumbers = { git = \"https://example.invalid/numbers.git\", rev = \"main\" }\n",
        "math/answer.aster",
        "namespace math; using numbers; public int Answer() { return Base() + 2; }",
    );
    let root = fixture.write_app("main");
    write(
        &fixture.app.join("Aster.toml"),
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n\n[dependencies]\nmath = { git = \"https://example.invalid/math.git\", rev = \"main\" }\nunrelated = { git = \"https://example.invalid/unrelated.git\", rev = \"main\" }\n",
    );

    assert!(fixture.aster(&["fetch"]).status.success());
    let lock_path = fixture.app.join("Aster.lock");
    let first = fs::read_to_string(&lock_path).expect("initial lockfile");
    let unrelated = locked_commit(&first, "unrelated");
    assert!(first.contains("name = \"numbers\""));

    write(
        &math.join("Aster.toml"),
        "[package]\nname = \"math\"\n\n[dependencies]\nreplacement = { git = \"https://example.invalid/replacement.git\", rev = \"main\" }\n",
    );
    write(
        &math.join("math/answer.aster"),
        "namespace math; using replacement; public int Answer() { return Base() + 2; }",
    );
    git(&math, ["add", "."]);
    git(&math, ["commit", "--quiet", "-m", "replace transitive"]);

    let updated = fixture.aster(&["fetch", "--update", "math"]);
    assert!(updated.status.success(), "{}", stderr(&updated));
    let second = fs::read_to_string(&lock_path).expect("updated lockfile");
    assert!(!second.contains("name = \"numbers\""));
    assert!(second.contains("name = \"replacement\""));
    assert_eq!(locked_commit(&second, "unrelated"), unrelated);
    let run = fixture.aster(&["run", root.to_str().expect("UTF-8 root")]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run).trim(), "43");
}

#[test]
fn fetch_supports_tags_full_commits_and_rejects_ambiguous_refs() {
    let fixture = Fixture::new("revisions");
    let math = fixture.repository(
        "math",
        "[package]\nname = \"math\"\n",
        "math/answer.aster",
        "namespace math; public int Answer() { return 42; }",
    );
    let commit = git_text(&math, ["rev-parse", "HEAD"]);
    git(&math, ["tag", "stable"]);

    fixture.write_app("stable");
    assert!(fixture.aster(&["fetch"]).status.success());
    assert!(
        fs::read_to_string(fixture.app.join("Aster.lock"))
            .expect("tag lock")
            .contains(&commit)
    );

    fixture.write_app(&commit);
    assert!(fixture.aster(&["fetch"]).status.success());
    git(&math, ["branch", "ambiguous"]);
    git(&math, ["tag", "ambiguous"]);
    fixture.write_app("ambiguous");
    let ambiguous = fixture.aster(&["fetch"]);
    assert_eq!(ambiguous.status.code(), Some(1));
    assert!(stderr(&ambiguous).contains("ambiguous"));
}

#[test]
fn stale_or_corrupt_local_state_fails_closed_and_fetch_repairs_only_the_cache() {
    let fixture = Fixture::new("fail-closed");
    let math = fixture.repository(
        "math",
        "[package]\nname = \"math\"\n",
        "math/answer.aster",
        "namespace math; public int Answer() { return 42; }",
    );
    write(&math.join(".gitignore"), "math/ignored.aster\n");
    git(&math, ["add", ".gitignore"]);
    git(
        &math,
        ["commit", "--quiet", "-m", "ignore generated source"],
    );
    fixture.write_app("main");
    assert!(fixture.aster(&["fetch"]).status.success());
    let lock_path = fixture.app.join("Aster.lock");
    let valid_lock = fs::read_to_string(&lock_path).expect("lockfile");

    fs::remove_file(&lock_path).expect("remove lock");
    assert!(stderr(&fixture.aster(&["check"])).contains("not locked"));
    write(
        &lock_path,
        &valid_lock.replace("rev = \"main\"", "rev = \"other\""),
    );
    assert!(stderr(&fixture.aster(&["check"])).contains("stale"));
    write(&lock_path, &valid_lock);

    let extra = format!(
        "{valid_lock}\n[[package]]\nname = \"unused\"\ngit = \"https://example.invalid/unused.git\"\nrev = \"main\"\ncommit = \"{}\"\n",
        "a".repeat(40)
    );
    write(&lock_path, &extra);
    let stale_extra = fixture.aster(&["check"]);
    assert_eq!(stale_extra.status.code(), Some(1));
    assert!(stderr(&stale_extra).contains("no longer in the Git dependency graph"));
    write(&lock_path, &valid_lock);

    let cache_entry = || {
        fs::read_dir(&fixture.cache)
            .expect("cache")
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().len() == 64)
            .expect("cache entry")
            .path()
    };
    fs::remove_dir_all(cache_entry()).expect("remove cache entry");
    let missing_cache = fixture.aster(&["check"]);
    assert_eq!(missing_cache.status.code(), Some(1));
    assert!(stderr(&missing_cache).contains("unavailable or corrupt"));
    assert!(fixture.aster(&["fetch"]).status.success());

    let cache_entry = cache_entry();
    write(
        &cache_entry.join("math/answer.aster"),
        "namespace math; public int Answer() { return 0; }",
    );
    let corrupt = fixture.aster(&["run"]);
    assert_eq!(corrupt.status.code(), Some(1));
    assert!(stderr(&corrupt).contains("corrupt"));
    assert!(fixture.aster(&["fetch"]).status.success());
    assert_eq!(stdout(&fixture.aster(&["run"])).trim(), "42");

    write(
        &cache_entry.join("math/ignored.aster"),
        "namespace math; public int Hidden() { return 1; }",
    );
    let ignored_corruption = fixture.aster(&["check"]);
    assert_eq!(ignored_corruption.status.code(), Some(1));
    assert!(stderr(&ignored_corruption).contains("corrupt"));
    assert!(fixture.aster(&["fetch"]).status.success());

    git(
        &cache_entry,
        ["update-index", "--assume-unchanged", "math/answer.aster"],
    );
    write(
        &cache_entry.join("math/answer.aster"),
        "namespace math; public int Answer() { return 0; }",
    );
    let hidden_corruption = fixture.aster(&["run"]);
    assert_eq!(hidden_corruption.status.code(), Some(1));
    assert!(stderr(&hidden_corruption).contains("corrupt"));
    assert!(fixture.aster(&["fetch"]).status.success());

    let unknown = fixture.aster(&["fetch", "--update", "unknown"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(stderr(&unknown).contains("was not found"));
}

#[test]
fn a_path_dependency_cannot_escape_a_materialized_git_source() {
    let fixture = Fixture::new("escape");
    fixture.repository(
        "math",
        "[package]\nname = \"math\"\n\n[dependencies]\nescape = { path = \"..\" }\n",
        "math/answer.aster",
        "namespace math; public int Answer() { return 42; }",
    );
    fixture.write_app("main");
    let output = fixture.aster(&["fetch"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("escapes the immutable Git package source"));
}

#[test]
fn a_git_package_can_use_a_path_package_inside_its_immutable_source() {
    let fixture = Fixture::new("mixed");
    let math = fixture.repository(
        "math",
        "[package]\nname = \"math\"\n\n[dependencies]\nhelper = { path = \"helper\" }\n",
        "math/answer.aster",
        "namespace math; using helper; public int Answer() { return Base() + 2; }",
    );
    write(
        &math.join("helper/Aster.toml"),
        "[package]\nname = \"helper\"\n",
    );
    write(
        &math.join("helper/helper/base.aster"),
        "namespace helper; public int Base() { return 40; }",
    );
    git(&math, ["add", "."]);
    git(&math, ["commit", "--quiet", "-m", "add path package"]);
    fixture.write_app("main");
    let fetch = fixture.aster(&["fetch"]);
    assert!(fetch.status.success(), "{}", stderr(&fetch));
    let lockfile = fs::read_to_string(fixture.app.join("Aster.lock")).expect("mixed lockfile");
    assert!(lockfile.contains("name = \"math\""));
    assert!(!lockfile.contains("name = \"helper\""));
    let run = fixture.aster(&["run"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run).trim(), "42");
}

#[test]
fn fetch_is_a_no_op_without_git_dependencies_and_rejects_path_updates() {
    let fixture = Fixture::new("no-git");
    write(
        &fixture.app.join("Aster.toml"),
        "[package]\nname = \"app\"\n",
    );
    let output = fixture.aster(&["fetch"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("No Git dependencies"));
    assert!(!fixture.app.join("Aster.lock").exists());

    let path = fixture.root.join("local");
    write(&path.join("Aster.toml"), "[package]\nname = \"local\"\n");
    write(
        &fixture.app.join("Aster.toml"),
        "[package]\nname = \"app\"\n\n[dependencies]\nlocal = { path = \"../local\" }\n",
    );
    let update = fixture.aster(&["fetch", "--update", "local"]);
    assert_eq!(update.status.code(), Some(1));
    assert!(stderr(&update).contains("not a Git dependency"));
}

#[test]
fn git_packages_reuse_name_and_cycle_validation_from_the_package_graph() {
    let mismatch = Fixture::new("name-mismatch");
    mismatch.repository(
        "math",
        "[package]\nname = \"other\"\n",
        "other/value.aster",
        "namespace other; public int Value() { return 1; }",
    );
    mismatch.write_app("main");
    let output = mismatch.aster(&["fetch"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("resolves to a package named `other`"));

    let cycle = Fixture::new("cycle");
    cycle.repository(
        "math",
        "[package]\nname = \"math\"\n\n[dependencies]\nmath = { git = \"https://example.invalid/math.git\", rev = \"main\" }\n",
        "math/value.aster",
        "namespace math; public int Value() { return 1; }",
    );
    cycle.write_app("main");
    let output = cycle.aster(&["fetch"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("dependency cycle"));
}

#[test]
fn gitlink_entries_are_rejected_even_without_a_gitmodules_file() {
    let fixture = Fixture::new("gitlink");
    let math = fixture.repository(
        "math",
        "[package]\nname = \"math\"\n",
        "math/value.aster",
        "namespace math; public int Value() { return 42; }",
    );
    let commit = git_text(&math, ["rev-parse", "HEAD"]);
    let cache_info = format!("160000,{commit},vendor");
    git(&math, ["update-index", "--add", "--cacheinfo", &cache_info]);
    git(&math, ["commit", "--quiet", "-m", "add gitlink"]);
    fixture.write_app("main");

    let output = fixture.aster(&["fetch"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("submodules are not supported"));
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, contents).expect("write fixture");
}

fn git<const N: usize>(directory: &Path, arguments: [&str; N]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run Git fixture command");
    assert!(output.status.success(), "{}", stderr(&output));
}

fn git_text<const N: usize>(directory: &Path, arguments: [&str; N]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run Git fixture command");
    assert!(output.status.success(), "{}", stderr(&output));
    stdout(&output).trim().to_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn locked_commit(lockfile: &str, package: &str) -> String {
    let marker = format!("name = \"{package}\"");
    let package = lockfile
        .split("[[package]]")
        .find(|entry| entry.contains(&marker))
        .expect("locked package");
    package
        .lines()
        .find_map(|line| line.strip_prefix("commit = \"")?.strip_suffix('"'))
        .expect("locked commit")
        .to_owned()
}
