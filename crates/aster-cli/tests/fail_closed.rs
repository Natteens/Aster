use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn temporary_source(label: &str, source: impl AsRef<[u8]>) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "aster-fail-closed-{label}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir(&directory).expect("create temporary source directory");
    let path = directory.join("main.aster");
    fs::write(&path, source).expect("write temporary Aster source");
    path
}

fn aster(command: &str, source: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aster"))
        .arg(command)
        .arg(source)
        .env_remove("ASTER_STDLIB")
        .output()
        .expect("run Aster CLI")
}

fn assert_controlled_failure(output: &Output, expected: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(stderr.contains(expected), "stderr={stderr}");
    for forbidden in [
        "thread 'main'",
        "overflowed its stack",
        "panicked",
        "stack backtrace",
        "memory allocation of",
    ] {
        assert!(!stderr.contains(forbidden), "stderr={stderr}");
    }
}

fn remove_source(path: &Path) {
    fs::remove_dir_all(path.parent().expect("source parent")).expect("remove temporary source");
}

#[test]
fn all_frontend_cli_paths_reject_pathological_nesting() {
    let nested = format!(
        "public class Program {{ public static int Main() {{ return {}1{}; }} }}",
        "(".repeat(256),
        ")".repeat(256)
    );
    let path = temporary_source("nesting", &nested);
    for command in ["check", "run", "dump-hir", "dump-mir"] {
        assert_controlled_failure(
            &aster(command, &path),
            "source nesting exceeds the compiler limit",
        );
    }
    remove_source(&path);
}

#[test]
fn impossible_positive_allocation_is_a_controlled_cli_failure() {
    let path = temporary_source(
        "allocation",
        "public class Program { public static int Main() { int[] values = new int[2147483647]; return values.Length; } }",
    );
    assert_controlled_failure(&aster("run", &path), "execution memory limit");
    remove_source(&path);
}

#[test]
fn excessive_recursion_is_a_controlled_cli_failure() {
    let path = temporary_source(
        "recursion",
        r"
            public class Program {
                public static int Recurse(int remaining) {
                    return remaining == 0 ? 0 : Recurse(remaining - 1) + 1;
                }
                public static int Main() { return Recurse(2000); }
            }
        ",
    );
    assert_controlled_failure(
        &aster("run", &path),
        "call depth exceeds the supported limit",
    );
    remove_source(&path);
}

#[test]
fn invalid_utf8_is_a_controlled_cli_failure() {
    let path = temporary_source("invalid-utf8", [0xff, 0xfe, 0xfd]);
    let root_output = aster("check", &path);
    assert_controlled_failure(&root_output, "valid UTF-8");
    let root_error = String::from_utf8_lossy(&root_output.stderr);
    assert!(root_error.contains("could not load root source file"));
    assert!(root_error.contains(path.to_string_lossy().as_ref()));
    assert!(!root_error.contains("namespace file"));
    assert!(!root_error.contains("fix the using"));
    remove_source(&path);

    let root = temporary_source(
        "invalid-namespace-utf8",
        "using app; public int Run() { return 0; }",
    );
    let namespace = root
        .parent()
        .expect("temporary root parent")
        .join("app/bad.aster");
    fs::create_dir_all(namespace.parent().expect("namespace parent"))
        .expect("create namespace directory");
    fs::write(&namespace, [0xff, 0xfe, 0xfd]).expect("write invalid namespace source");
    let namespace_output = aster("check", &root);
    assert_controlled_failure(&namespace_output, "valid UTF-8");
    let namespace_error = String::from_utf8_lossy(&namespace_output.stderr);
    assert!(namespace_error.contains("could not load namespace file"));
    assert!(namespace_error.contains("bad.aster"));
    assert!(namespace_error.contains("fix the using"));
    remove_source(&root);
}
