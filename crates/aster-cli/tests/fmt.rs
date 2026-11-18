use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

const MESSY: &str = "public int Run() {\n  int x=1; // keep\n    return x;\n}";
const CANONICAL: &str = "public int Run()\n{\n    int x = 1; // keep\n    return x;\n}\n";

#[test]
fn fmt_rewrites_files_in_place_and_is_idempotent() {
    let directory = temporary_directory("rewrite");
    let first = directory.join("a.aster");
    let second = directory.join("b.aster");
    fs::write(&first, MESSY).expect("write a");
    fs::write(&second, CANONICAL).expect("write b");

    let output = aster(&directory, ["fmt", "a.aster", "b.aster"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(&first).expect("read a"), CANONICAL);
    assert_eq!(fs::read_to_string(&second).expect("read b"), CANONICAL);
    assert!(stdout(&output).contains("a.aster"));
    assert!(!stdout(&output).contains("b.aster"));

    let again = aster(&directory, ["fmt", "a.aster", "b.aster"]);
    assert_eq!(again.status.code(), Some(0));
    assert_eq!(stdout(&again), "");
    assert_eq!(fs::read_to_string(&first).expect("read a"), CANONICAL);
}

#[test]
fn fmt_check_lists_files_and_exits_nonzero_without_writing() {
    let directory = temporary_directory("check");
    let messy = directory.join("messy.aster");
    let clean = directory.join("clean.aster");
    fs::write(&messy, MESSY).expect("write messy");
    fs::write(&clean, CANONICAL).expect("write clean");

    let output = aster(&directory, ["fmt", "--check", "messy.aster", "clean.aster"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("messy.aster"));
    assert!(!stdout(&output).contains("clean.aster"));
    assert_eq!(fs::read_to_string(&messy).expect("read"), MESSY);

    let ok = aster(&directory, ["fmt", "--check", "clean.aster"]);
    assert_eq!(ok.status.code(), Some(0));
    assert_eq!(stdout(&ok), "");
}

#[test]
fn fmt_expands_directories_and_skips_target() {
    let directory = temporary_directory("directory");
    fs::create_dir_all(directory.join("app")).expect("app");
    fs::create_dir_all(directory.join("target")).expect("target");
    fs::write(directory.join("app").join("main.aster"), MESSY).expect("main");
    fs::write(directory.join("target").join("skip.aster"), MESSY).expect("skip");

    let output = aster(&directory, ["fmt", "."]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        fs::read_to_string(directory.join("app").join("main.aster")).expect("read"),
        CANONICAL
    );
    assert_eq!(
        fs::read_to_string(directory.join("target").join("skip.aster")).expect("read"),
        MESSY
    );
}

#[test]
fn fmt_without_files_formats_the_current_project() {
    let directory = temporary_directory("project");
    fs::write(directory.join("Aster.toml"), "[package]\nname = \"demo\"\n").expect("manifest");
    fs::create_dir_all(directory.join("app")).expect("app");
    fs::write(directory.join("app").join("main.aster"), MESSY).expect("main");

    let output = aster(&directory, ["fmt", "--check"]);
    assert_eq!(output.status.code(), Some(1));
    let output = aster(&directory, ["fmt"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let output = aster(&directory, ["fmt", "--check"]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn fmt_reports_lexical_errors_and_leaves_the_file_alone() {
    let directory = temporary_directory("lex-error");
    let broken = directory.join("broken.aster");
    fs::write(&broken, "void F() { string s = \"open; }").expect("write");
    let output = aster(&directory, ["fmt", "broken.aster"]);
    assert_eq!(output.status.code(), Some(1));
    assert_ne!(stderr(&output), "");
    assert_eq!(
        fs::read_to_string(&broken).expect("read"),
        "void F() { string s = \"open; }"
    );
}

#[test]
fn fmt_usage_errors_exit_with_two() {
    let directory = temporary_directory("usage");
    let output = aster(&directory, ["fmt", "--indent=2"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("aster fmt [--check] [FILE...]"));

    let missing = aster(&directory, ["fmt", "missing.aster"]);
    assert_eq!(missing.status.code(), Some(1));

    let help = aster(&directory, ["fmt", "--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout(&help).contains("Usage: aster fmt [--check] [FILE...]"));
    assert!(stdout(&aster(&directory, ["--help"])).contains("fmt [FILE...]"));
}

fn aster<const N: usize>(directory: &Path, arguments: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aster"))
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run aster")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn temporary_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "aster-fmt-{label}-{}-{}",
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("create temporary directory");
    directory
}
