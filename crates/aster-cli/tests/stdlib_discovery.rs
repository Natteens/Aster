//! Integration tests for stdlib discovery: `ASTER_STDLIB`, exe-relative
//! install layout, and the embedded dev fallback.
//!
//! All env-var injection is done per-subprocess via `Command::env` so parallel
//! test execution cannot observe another test's environment.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const ASTER_STDLIB_ENV: &str = "ASTER_STDLIB";

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

// Helpers

fn temp_dir(label: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "aster-stdlib-discovery-{label}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp directory");
    dir
}

/// Copy the real stdlib from the workspace into `root/aster/`.
fn copy_real_stdlib(root: &std::path::Path) {
    let workspace_stdlib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../stdlib");
    let modules: &[&str] = &[
        "aster/math.aster",
        "aster/text/text.aster",
        "aster/core/core.aster",
        "aster/io/io.aster",
        "aster/collections/collections.aster",
        "aster/testing/testing.aster",
    ];
    for relative in modules {
        let src = workspace_stdlib.join(relative);
        let dst = root.join(relative);
        fs::create_dir_all(dst.parent().expect("parent")).expect("create dir");
        fs::copy(&src, &dst).unwrap_or_else(|e| panic!("copy {relative}: {e}"));
    }
}

fn aster_with_env(
    args: &[&str],
    env_key: &str,
    env_val: &str,
    cwd: Option<&std::path::Path>,
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aster"));
    cmd.args(args).env(env_key, env_val);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.output().expect("run Aster binary")
}

fn aster_without_env(args: &[&str], cwd: Option<&std::path::Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aster"));
    cmd.args(args).env_remove(ASTER_STDLIB_ENV);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.output().expect("run Aster binary")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

/// Minimal source that uses the stdlib (requires aster.math to be present).
const STDLIB_PROGRAM: &str = "using aster.math; public class Program { public static int Main() { return Math.Max(0, 1); } }";

// ASTER_STDLIB tests (run against the subprocess, not the in-process env)

#[test]
fn aster_stdlib_valid_path_compiles_and_runs() {
    let root = temp_dir("env-valid");
    copy_real_stdlib(&root);
    let project = temp_dir("env-valid-project");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let output = aster_with_env(
        &["run", src.to_str().expect("UTF-8")],
        ASTER_STDLIB_ENV,
        root.to_str().expect("UTF-8 stdlib root"),
        None,
    );
    fs::remove_dir_all(&root).ok();
    fs::remove_dir_all(&project).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "1");
}

#[test]
fn aster_stdlib_nonexistent_path_fails_with_nonzero_exit() {
    let nonexistent = temp_dir("env-nonexistent");
    fs::remove_dir_all(&nonexistent).ok(); // ensure it doesn't exist

    let project = temp_dir("env-nonexistent-project");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let output = aster_with_env(
        &["run", src.to_str().expect("UTF-8")],
        ASTER_STDLIB_ENV,
        nonexistent.to_str().expect("UTF-8"),
        None,
    );
    fs::remove_dir_all(&project).ok();

    assert!(
        !output.status.success(),
        "must exit non-zero for missing stdlib"
    );
}

#[test]
fn aster_stdlib_pointing_to_file_fails() {
    let dir = temp_dir("env-file");
    let file = dir.join("not-a-dir");
    fs::write(&file, "content").expect("write file");

    let project = temp_dir("env-file-project");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let output = aster_with_env(
        &["run", src.to_str().expect("UTF-8")],
        ASTER_STDLIB_ENV,
        file.to_str().expect("UTF-8"),
        None,
    );
    fs::remove_dir_all(&dir).ok();
    fs::remove_dir_all(&project).ok();

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("file, not a directory")
            || stderr(&output).contains("not accessible")
            || stderr(&output).contains("invalid"),
        "unexpected error: {}",
        stderr(&output)
    );
}

#[test]
fn aster_stdlib_empty_directory_fails() {
    let root = temp_dir("env-empty");
    // root exists but has no content

    let project = temp_dir("env-empty-project");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let output = aster_with_env(
        &["run", src.to_str().expect("UTF-8")],
        ASTER_STDLIB_ENV,
        root.to_str().expect("UTF-8"),
        None,
    );
    fs::remove_dir_all(&root).ok();
    fs::remove_dir_all(&project).ok();

    assert!(!output.status.success(), "empty stdlib dir must fail");
}

#[test]
fn aster_stdlib_incomplete_structure_fails() {
    let root = temp_dir("env-incomplete");
    // Only write math — missing core, text, io, collections.
    let math = root.join("aster/math.aster");
    fs::create_dir_all(math.parent().expect("parent")).expect("mkdir");
    fs::write(&math, "").expect("write math");

    let project = temp_dir("env-incomplete-project");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let output = aster_with_env(
        &["run", src.to_str().expect("UTF-8")],
        ASTER_STDLIB_ENV,
        root.to_str().expect("UTF-8"),
        None,
    );
    fs::remove_dir_all(&root).ok();
    fs::remove_dir_all(&project).ok();

    assert!(!output.status.success(), "incomplete stdlib must fail");
}

#[test]
fn aster_stdlib_path_with_spaces_works() {
    let root = temp_dir("env valid with spaces");
    copy_real_stdlib(&root);
    let project = temp_dir("env-spaces-project");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let output = aster_with_env(
        &["run", src.to_str().expect("UTF-8")],
        ASTER_STDLIB_ENV,
        root.to_str().expect("UTF-8"),
        None,
    );
    fs::remove_dir_all(&root).ok();
    fs::remove_dir_all(&project).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn aster_stdlib_unicode_path_works() {
    let root = temp_dir("env-stdlib-üñïcödé");
    copy_real_stdlib(&root);
    let project = temp_dir("env-unicode-project");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let output = aster_with_env(
        &["run", src.to_str().expect("UTF-8")],
        ASTER_STDLIB_ENV,
        root.to_str().expect("UTF-8"),
        None,
    );
    fs::remove_dir_all(&root).ok();
    fs::remove_dir_all(&project).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

// cwd independence: stdlib works when cwd is completely unrelated

#[test]
fn embedded_stdlib_works_regardless_of_working_directory() {
    let project = temp_dir("cwd-independence");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    // Run from the OS temp directory root — not the repo, not the project dir.
    let output = aster_without_env(
        &["run", src.to_str().expect("UTF-8")],
        Some(&std::env::temp_dir()),
    );
    fs::remove_dir_all(&project).ok();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "1");
}

// Priority: ASTER_STDLIB beats embedded fallback

#[test]
fn aster_stdlib_invalid_takes_priority_over_embedded_fallback() {
    // Set ASTER_STDLIB to a nonexistent path — this must fail even though the
    // embedded fallback would succeed. Proves env-var is consulted first.
    let nonexistent = temp_dir("priority-nonexistent");
    fs::remove_dir_all(&nonexistent).ok();

    let project = temp_dir("priority-project");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let output = aster_with_env(
        &["run", src.to_str().expect("UTF-8")],
        ASTER_STDLIB_ENV,
        nonexistent.to_str().expect("UTF-8"),
        None,
    );
    fs::remove_dir_all(&project).ok();

    assert!(
        !output.status.success(),
        "ASTER_STDLIB with invalid path must fail, not silently fall back to embedded"
    );
}

// Error message content

#[test]
fn aster_stdlib_error_message_mentions_env_var() {
    let nonexistent = temp_dir("msg-nonexistent");
    fs::remove_dir_all(&nonexistent).ok();

    let project = temp_dir("msg-project");
    let src = project.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let output = aster_with_env(
        &["run", src.to_str().expect("UTF-8")],
        ASTER_STDLIB_ENV,
        nonexistent.to_str().expect("UTF-8"),
        None,
    );
    fs::remove_dir_all(&project).ok();

    let err = stderr(&output);
    assert!(
        err.contains("ASTER_STDLIB"),
        "error message must mention ASTER_STDLIB, got: {err}"
    );
}

// Relocatable proof: exe-relative stdlib discovery
// The copied binary runs from an unrelated project directory without
// ASTER_STDLIB, proving it resolves ../stdlib relative to its executable.

#[test]
#[allow(clippy::too_many_lines, clippy::similar_names)]
fn exe_relative_stdlib_discovery_works_outside_repo() {
    let root = temp_dir("relocatable");

    let bin_dir = root.join("bin");
    let stdlib_dir = root.join("stdlib");
    let project_dir = root.join("project");
    fs::create_dir_all(&bin_dir).expect("create bin dir");
    fs::create_dir_all(&stdlib_dir).expect("create stdlib dir");
    fs::create_dir_all(&project_dir).expect("create project dir");

    let real_binary = PathBuf::from(env!("CARGO_BIN_EXE_aster"));
    let binary_filename = real_binary.file_name().expect("binary filename");
    let copied_binary = bin_dir.join(binary_filename);
    fs::copy(&real_binary, &copied_binary).unwrap_or_else(|e| panic!("copy binary: {e}"));

    copy_real_stdlib(&stdlib_dir);

    let src = project_dir.join("main.aster");
    fs::write(&src, STDLIB_PROGRAM).expect("write source");

    let src_path = src.to_str().expect("UTF-8 source path");

    let check = Command::new(&copied_binary)
        .args(["check", src_path])
        .current_dir(&project_dir)
        .env_remove(ASTER_STDLIB_ENV)
        .output()
        .expect("run aster check");
    assert!(
        check.status.success(),
        "aster check failed (exe-relative stdlib not found):\n{}",
        stderr(&check)
    );

    let dump_hir = Command::new(&copied_binary)
        .args(["dump-hir", src_path])
        .current_dir(&project_dir)
        .env_remove(ASTER_STDLIB_ENV)
        .output()
        .expect("run aster dump-hir");
    assert!(
        dump_hir.status.success(),
        "aster dump-hir failed:\n{}",
        stderr(&dump_hir)
    );

    let dump_mir = Command::new(&copied_binary)
        .args(["dump-mir", src_path])
        .current_dir(&project_dir)
        .env_remove(ASTER_STDLIB_ENV)
        .output()
        .expect("run aster dump-mir");
    assert!(
        dump_mir.status.success(),
        "aster dump-mir failed:\n{}",
        stderr(&dump_mir)
    );

    let run = Command::new(&copied_binary)
        .args(["run", src_path])
        .current_dir(&project_dir)
        .env_remove(ASTER_STDLIB_ENV)
        .output()
        .expect("run aster run");
    assert!(
        run.status.success(),
        "aster run failed (exe-relative stdlib not found):\n{}",
        stderr(&run)
    );
    assert_eq!(
        stdout(&run).trim(),
        "1",
        "unexpected program output: {}",
        stdout(&run)
    );

    fs::remove_dir_all(&root).ok();
}
