use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn temporary_source(label: &str, source: &str) -> PathBuf {
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

fn temporary_source_bytes(label: &str, source: &[u8]) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "aster-fail-closed-{label}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir(&directory).expect("create temporary source directory");
    let path = directory.join("main.aster");
    fs::write(&path, source).expect("write temporary Aster source bytes");
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
