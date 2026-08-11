use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

const MANIFEST: &str = "schema = 2\n\n[package]\nname = \"HelloAster\"\n\n[application]\nentry = \"app.Program.Main\"\n";
const SOURCE: &str = r#"namespace app;

using aster.io;

public class Program
{
    public static int Main()
    {
        WriteLine("Hello from ASTER!");
        return 0;
    }
}
"#;

#[test]
fn new_project_is_functional_outside_the_checkout() {
    let parent = temporary_directory("functional parent ü");
    let created = aster(&parent, ["new", "HelloAster"]);
    assert!(created.status.success(), "{}", stderr(&created));
    let output = stdout(&created);
    assert!(output.contains("ASTER project created"));
    assert!(output.contains("Name: HelloAster"));
    assert!(output.contains("aster check"));
    assert!(output.contains("aster run"));

    let project = parent.join("HelloAster");
    assert_eq!(project_files(&project), ["Aster.toml", "app/main.aster"]);
    assert_eq!(
        fs::read_to_string(project.join("Aster.toml")).expect("read manifest"),
        MANIFEST
    );
    assert_eq!(
        fs::read_to_string(project.join("app/main.aster")).expect("read source"),
        SOURCE
    );

    for command in ["check", "dump-hir", "dump-mir"] {
        let result = aster(&project, [command]);
        assert!(result.status.success(), "{command}: {}", stderr(&result));
    }
    let run = aster(&project, ["run"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(stdout(&run).contains("Hello from ASTER!"));
    assert_eq!(run.status.code(), Some(0));

    fs::remove_dir_all(parent).expect("remove functional test directory");
}

#[test]
fn unicode_project_name_uses_a_neutral_valid_namespace() {
    let parent = temporary_directory("unicode-name");
    let output = aster(&parent, ["new", "Projeto Áster"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let project = parent.join("Projeto Áster");
    assert_eq!(
        fs::read_to_string(project.join("app/main.aster")).expect("read source"),
        SOURCE
    );
    let check = aster(&project, ["check"]);
    assert!(check.status.success(), "{}", stderr(&check));
    fs::remove_dir_all(parent).expect("remove Unicode test directory");
}

#[test]
fn invalid_names_are_rejected_without_writing() {
    let parent = temporary_directory("invalid-names");
    let absolute = parent.join("absolute").to_string_lossy().into_owned();
    let long = "a".repeat(65);
    let cases = [
        "",
        ".",
        "..",
        "../escape",
        "nested/project",
        "nested\\project",
        "C:\\absolute",
        "\\\\server\\share",
        "bad\u{1f}name",
        "CON",
        "nul.txt",
        ".hidden",
        "trailing.",
        absolute.as_str(),
        long.as_str(),
    ];
    for name in cases {
        let output = aster(&parent, ["new", name]);
        assert!(!output.status.success(), "{name:?} unexpectedly succeeded");
    }
    assert!(fs::read_dir(&parent).expect("read parent").next().is_none());
    fs::remove_dir_all(parent).expect("remove invalid-name directory");
}

#[test]
fn missing_and_extra_arguments_have_focused_errors() {
    let parent = temporary_directory("arguments");
    let missing = aster(&parent, ["new"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("usage: aster new <NAME>"));

    let extra = aster(&parent, ["new", "One", "Two"]);
    assert!(!extra.status.success());
    assert!(stderr(&extra).contains("unexpected argument `Two`"));

    let help = aster(&parent, ["new", "--help"]);
    assert!(help.status.success());
    assert!(stdout(&help).contains("Usage: aster new <NAME>"));
    assert!(fs::read_dir(&parent).expect("read parent").next().is_none());
    fs::remove_dir_all(parent).expect("remove argument directory");
}

#[test]
fn existing_destinations_are_preserved_byte_for_byte() {
    let parent = temporary_directory("existing-destinations");

    let file = parent.join("FileProject");
    fs::write(&file, "user data").expect("write destination file");
    let file_result = aster(&parent, ["new", "FileProject"]);
    assert!(!file_result.status.success());
    assert_eq!(
        fs::read_to_string(&file).expect("read destination file"),
        "user data"
    );

    let empty = parent.join("EmptyProject");
    fs::create_dir(&empty).expect("create empty destination");
    let empty_result = aster(&parent, ["new", "EmptyProject"]);
    assert!(!empty_result.status.success());
    assert!(empty.is_dir());

    let owned = parent.join("OwnedProject");
    fs::create_dir(&owned).expect("create owned destination");
    fs::create_dir(owned.join(".git")).expect("create hidden repository");
    fs::write(owned.join("Aster.toml"), "user manifest").expect("write user manifest");
    let before = snapshot(&owned);
    let owned_result = aster(&parent, ["new", "OwnedProject"]);
    assert!(!owned_result.status.success());
    assert!(
        stderr(&owned_result).contains("destination directory is not empty"),
        "{}",
        stderr(&owned_result)
    );
    assert_eq!(snapshot(&owned), before);
    assert_no_staging(&parent);

    fs::remove_dir_all(parent).expect("remove existing-destination directory");
}

#[test]
fn repeated_creation_is_not_an_update_and_preserves_the_project() {
    let parent = temporary_directory("idempotence");
    let first = aster(&parent, ["new", "HelloAster"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let project = parent.join("HelloAster");
    let before = snapshot(&project);

    let second = aster(&parent, ["new", "HelloAster"]);
    assert!(!second.status.success());
    assert_eq!(snapshot(&project), before);
    assert_no_staging(&parent);
    fs::remove_dir_all(parent).expect("remove idempotence directory");
}

#[test]
fn generation_is_deterministic_across_different_parents() {
    let first_parent = temporary_directory("determinism one");
    let second_parent = temporary_directory("determinism two ü");
    assert!(
        aster(&first_parent, ["new", "SameProject"])
            .status
            .success()
    );
    assert!(
        aster(&second_parent, ["new", "SameProject"])
            .status
            .success()
    );

    let first = snapshot(&first_parent.join("SameProject"));
    let second = snapshot(&second_parent.join("SameProject"));
    assert_eq!(first, second);

    fs::remove_dir_all(first_parent).expect("remove first deterministic parent");
    fs::remove_dir_all(second_parent).expect("remove second deterministic parent");
}

#[test]
fn global_help_advertises_new_without_changing_unknown_command_errors() {
    let parent = temporary_directory("help");
    let help = aster(&parent, ["--help"]);
    assert!(help.status.success());
    assert!(stdout(&help).contains("new <NAME>"));

    let unknown = aster(&parent, ["unknown"]);
    assert!(!unknown.status.success());
    assert!(stderr(&unknown).contains("unknown command `unknown`"));
    fs::remove_dir_all(parent).expect("remove help directory");
}

#[cfg(unix)]
#[test]
fn symlink_destination_is_rejected_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let parent = temporary_directory("symlink");
    let outside = temporary_directory("symlink-outside");
    fs::write(outside.join("owned.txt"), "owned").expect("write outside file");
    symlink(&outside, parent.join("LinkedProject")).expect("create destination symlink");

    let output = aster(&parent, ["new", "LinkedProject"]);
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(outside.join("owned.txt")).expect("read outside file"),
        "owned"
    );
    fs::remove_dir_all(parent).expect("remove symlink parent");
    fs::remove_dir_all(outside).expect("remove symlink target");
}

#[cfg(windows)]
#[test]
fn reparse_destination_is_rejected_when_symlink_creation_is_available() {
    use std::os::windows::fs::symlink_dir;

    let parent = temporary_directory("reparse");
    let outside = temporary_directory("reparse-outside");
    fs::write(outside.join("owned.txt"), "owned").expect("write outside file");
    if symlink_dir(&outside, parent.join("LinkedProject")).is_ok() {
        let output = aster(&parent, ["new", "LinkedProject"]);
        assert!(!output.status.success());
        assert_eq!(
            fs::read_to_string(outside.join("owned.txt")).expect("read outside file"),
            "owned"
        );
    }
    fs::remove_dir_all(parent).expect("remove reparse parent");
    fs::remove_dir_all(outside).expect("remove reparse target");
}

fn aster<const N: usize>(current_directory: &Path, arguments: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aster"))
        .args(arguments)
        .current_dir(current_directory)
        .env_remove("ASTER_STDLIB")
        .output()
        .expect("run ASTER CLI")
}

fn temporary_directory(label: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-new-integration-{label}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create temporary directory");
    path
}

fn project_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files);
    files.sort();
    files
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<String>) {
    for entry in fs::read_dir(directory).expect("read project directory") {
        let entry = entry.expect("read project entry");
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, files);
        } else {
            files.push(
                path.strip_prefix(root)
                    .expect("project-relative path")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    project_files(root)
        .into_iter()
        .map(|relative| {
            let bytes = fs::read(root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
                .expect("read snapshot file");
            (relative, bytes)
        })
        .collect()
}

fn assert_no_staging(parent: &Path) {
    assert!(fs::read_dir(parent).expect("read parent").all(|entry| {
        !entry
            .expect("read entry")
            .file_name()
            .to_string_lossy()
            .contains(".aster-new-")
    }));
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr")
}
