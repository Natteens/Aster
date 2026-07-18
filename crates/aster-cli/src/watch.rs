//! `aster watch FILE [--function NAME]`: recompile and restart on file change.
//!
//! This is deliberately *not* hot reload: no state survives a rebuild. The
//! watcher polls file metadata (no extra dependencies, identical behavior on
//! Windows and Unix) and debounces by waiting for two consecutive identical
//! snapshots before rebuilding, so one editor save triggers one rebuild.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Observable state of the watched file at one instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    modified: Option<SystemTime>,
    length: u64,
}

pub(crate) fn file_snapshot(path: &Path) -> Option<Snapshot> {
    let metadata = fs::metadata(path).ok()?;
    Some(Snapshot {
        modified: metadata.modified().ok(),
        length: metadata.len(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WatchDecision {
    Wait,
    Rebuild,
}

/// Debouncing change detector, fed one snapshot per poll. A rebuild fires only
/// when the file differs from the last built state *and* has been stable for
/// two consecutive observations, so partially written saves are skipped.
#[derive(Debug, Default)]
#[cfg(test)]
pub(crate) struct ChangeDetector {
    last_built: Option<Snapshot>,
    pending: Option<Snapshot>,
}

#[cfg(test)]
impl ChangeDetector {
    pub(crate) fn built(snapshot: Option<Snapshot>) -> Self {
        Self {
            last_built: snapshot,
            pending: None,
        }
    }

    pub(crate) fn observe(&mut self, current: Option<Snapshot>) -> WatchDecision {
        let Some(current) = current else {
            // The file is briefly missing mid-save (or was deleted); keep waiting.
            self.pending = None;
            return WatchDecision::Wait;
        };
        if self.last_built.as_ref() == Some(&current) {
            self.pending = None;
            return WatchDecision::Wait;
        }
        if self.pending.as_ref() == Some(&current) {
            self.last_built = Some(current);
            self.pending = None;
            WatchDecision::Rebuild
        } else {
            self.pending = Some(current);
            WatchDecision::Wait
        }
    }
}

/// Watch a file, rebuilding and re-running the selected function after every
/// stable change. Errors are reported without stopping the watcher; `Ctrl+C`
/// terminates the process. Runs until interrupted.
pub(crate) fn watch_file(file_name: &str, function_name: Option<&str>) -> Result<(), ()> {
    let path = Path::new(file_name).to_owned();
    // Validate readability once so an unusable path fails fast.
    crate::read_source(file_name)?;
    println!("[watch] watching `{file_name}` — press Ctrl+C to stop");
    let initial = build_and_run(file_name, function_name);
    let mut failing = !initial.succeeded;
    let mut dependencies = initial.dependencies.unwrap_or_else(|| vec![path.clone()]);
    let mut detector = DependencyChangeDetector::built(dependency_snapshot(&dependencies));
    loop {
        thread::sleep(POLL_INTERVAL);
        if detector.observe(dependency_snapshot(&dependencies)) == WatchDecision::Rebuild {
            println!("[watch] change detected, rebuilding");
            let rebuilt = build_and_run(file_name, function_name);
            let succeeded = rebuilt.succeeded;
            if let Some(new_dependencies) = rebuilt.dependencies {
                dependencies = new_dependencies;
                detector = DependencyChangeDetector::built(dependency_snapshot(&dependencies));
            }
            if succeeded && failing {
                println!("[watch] compilation succeeded again");
            }
            failing = !succeeded;
        }
    }
}

/// One recompile-and-run cycle sharing the exact `run` pipeline. Reports Aster
/// frontend and JIT+execution times; no Cargo build time is involved here.
struct BuildOutcome {
    succeeded: bool,
    dependencies: Option<Vec<PathBuf>>,
}

fn build_and_run(file_name: &str, function_name: Option<&str>) -> BuildOutcome {
    let frontend_started = Instant::now();
    let project = match aster_compiler::compile_project(Path::new(file_name)) {
        Ok(compilation) => compilation,
        Err(diagnostics) => {
            for diagnostic in diagnostics {
                eprintln!("{}", diagnostic.render());
            }
            eprintln!("[watch] compilation failed; still watching");
            return BuildOutcome {
                succeeded: false,
                // A failed rebuild must keep the last successful graph. Otherwise an
                // invalid save in a namespace dependency would remove that file from the
                // watch set and fixing it would never trigger recovery.
                dependencies: None,
            };
        }
    };
    let frontend_time = frontend_started.elapsed();
    for diagnostic in crate::project_diagnostics(&project) {
        eprintln!("{}", diagnostic.render());
    }
    let execution_started = Instant::now();
    let dependencies = Some(watched_paths(&project, Path::new(file_name)));
    if let Ok((value, entry_name)) =
        crate::execute_project(&project, Path::new(file_name), function_name)
    {
        let execution_time = execution_started.elapsed();
        println!(
            "[watch] compiled in {:.1} ms, JIT+run in {:.1} ms",
            frontend_time.as_secs_f64() * 1000.0,
            execution_time.as_secs_f64() * 1000.0
        );
        println!("[watch] `{entry_name}` => {value}");
        BuildOutcome {
            succeeded: true,
            dependencies,
        }
    } else {
        eprintln!("[watch] execution failed; still watching");
        BuildOutcome {
            succeeded: false,
            dependencies,
        }
    }
}

fn watched_paths(project: &aster_compiler::ProjectCompilation, root_file: &Path) -> Vec<PathBuf> {
    let mut paths = project.dependency_paths();
    if let Some(manifest) = aster_compiler::find_manifest_path(root_file) {
        paths.push(manifest);
    }
    paths.sort();
    paths.dedup();
    paths
}

type DependencySnapshot = BTreeMap<PathBuf, Option<Snapshot>>;

fn dependency_snapshot(paths: &[PathBuf]) -> DependencySnapshot {
    paths
        .iter()
        .map(|path| (path.clone(), file_snapshot(path)))
        .collect()
}

#[derive(Debug)]
struct DependencyChangeDetector {
    last_built: DependencySnapshot,
    pending: Option<DependencySnapshot>,
}

impl DependencyChangeDetector {
    fn built(snapshot: DependencySnapshot) -> Self {
        Self {
            last_built: snapshot,
            pending: None,
        }
    }

    fn observe(&mut self, current: DependencySnapshot) -> WatchDecision {
        if current == self.last_built {
            self.pending = None;
            return WatchDecision::Wait;
        }
        if self.pending.as_ref() == Some(&current) {
            self.last_built = current;
            self.pending = None;
            WatchDecision::Rebuild
        } else {
            self.pending = Some(current);
            WatchDecision::Wait
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::PathBuf,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::{
        ChangeDetector, DependencyChangeDetector, Snapshot, WatchDecision, build_and_run,
        watched_paths,
    };

    #[allow(clippy::unnecessary_wraps)] // matches `ChangeDetector::observe`'s input
    fn snapshot(seconds: u64, length: u64) -> Option<Snapshot> {
        Some(Snapshot {
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
            length,
        })
    }

    #[test]
    fn unchanged_file_never_rebuilds() {
        let mut detector = ChangeDetector::built(snapshot(1, 10));
        for _ in 0..5 {
            assert_eq!(detector.observe(snapshot(1, 10)), WatchDecision::Wait);
        }
    }

    #[test]
    fn one_save_produces_one_rebuild_after_debounce() {
        let mut detector = ChangeDetector::built(snapshot(1, 10));
        assert_eq!(detector.observe(snapshot(2, 12)), WatchDecision::Wait);
        assert_eq!(detector.observe(snapshot(2, 12)), WatchDecision::Rebuild);
        assert_eq!(detector.observe(snapshot(2, 12)), WatchDecision::Wait);
    }

    #[test]
    fn rapid_successive_writes_debounce_into_one_rebuild() {
        let mut detector = ChangeDetector::built(snapshot(1, 10));
        assert_eq!(detector.observe(snapshot(2, 5)), WatchDecision::Wait);
        assert_eq!(detector.observe(snapshot(3, 8)), WatchDecision::Wait);
        assert_eq!(detector.observe(snapshot(4, 12)), WatchDecision::Wait);
        assert_eq!(detector.observe(snapshot(4, 12)), WatchDecision::Rebuild);
    }

    #[test]
    fn missing_file_waits_and_recovers() {
        let mut detector = ChangeDetector::built(snapshot(1, 10));
        assert_eq!(detector.observe(None), WatchDecision::Wait);
        assert_eq!(detector.observe(snapshot(2, 11)), WatchDecision::Wait);
        assert_eq!(detector.observe(snapshot(2, 11)), WatchDecision::Rebuild);
    }

    #[test]
    fn further_changes_after_rebuild_trigger_again() {
        let mut detector = ChangeDetector::built(snapshot(1, 10));
        detector.observe(snapshot(2, 11));
        assert_eq!(detector.observe(snapshot(2, 11)), WatchDecision::Rebuild);
        assert_eq!(detector.observe(snapshot(3, 12)), WatchDecision::Wait);
        assert_eq!(detector.observe(snapshot(3, 12)), WatchDecision::Rebuild);
    }

    #[test]
    fn namespace_file_changes_trigger_a_project_rebuild() {
        let root = PathBuf::from("main.aster");
        let dependency = PathBuf::from("app/math.aster");
        let initial = BTreeMap::from([
            (root.clone(), snapshot(1, 10)),
            (dependency.clone(), snapshot(1, 20)),
        ]);
        let mut changed = initial.clone();
        changed.insert(dependency, snapshot(2, 21));
        let mut detector = DependencyChangeDetector::built(initial);
        assert_eq!(detector.observe(changed.clone()), WatchDecision::Wait);
        assert_eq!(detector.observe(changed), WatchDecision::Rebuild);
    }

    #[test]
    fn conventional_entry_runs_and_manifest_is_watched() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("aster-watch-main-{nonce}"));
        fs::create_dir_all(&directory).expect("create test directory");
        let manifest = directory.join("Aster.toml");
        fs::write(&manifest, "[application]\nentry = \"app.Program.Main\"\n")
            .expect("write manifest");
        let root = directory.join("app/main.aster");
        fs::create_dir_all(root.parent().expect("root parent")).expect("create namespace");
        fs::write(
            &root,
            "namespace app; public class Program { public static int Main() { return 42; } }",
        )
        .expect("write source");
        let outcome = build_and_run(root.to_str().expect("UTF-8 path"), None);
        assert!(outcome.succeeded);
        let project = aster_compiler::compile_project(&root).expect("compile test project");
        assert!(
            watched_paths(&project, &root)
                .contains(&fs::canonicalize(&manifest).expect("canonical manifest path"))
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn bundled_standard_library_is_not_a_watched_project_file() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("examples/math_basics.aster");
        let project = aster_compiler::compile_project(&root).expect("compile math example");
        let paths = watched_paths(&project, &root);
        assert_eq!(paths, vec![fs::canonicalize(root).expect("canonical root")]);
    }

    #[test]
    fn string_rebuilds_use_fresh_execution_contexts() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("aster-watch-strings-{nonce}"));
        fs::create_dir_all(&directory).expect("create test directory");
        let root = directory.join("main.aster");
        fs::write(
            &root,
            r#"public class Program { public static int Main() { string text = "Ol" + "á"; return text.Length; } }"#,
        )
        .expect("write first string program");
        assert!(build_and_run(root.to_str().expect("UTF-8 path"), None).succeeded);
        fs::write(
            &root,
            r#"public class Program { public static int Main() { string text = "Ast" + "er"; return text.Length; } }"#,
        )
        .expect("write rebuilt string program");
        assert!(build_and_run(root.to_str().expect("UTF-8 path"), None).succeeded);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn failed_rebuild_keeps_the_last_successful_namespace_graph() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("aster-watch-recovery-{nonce}"));
        let dependency = directory.join("app/value.aster");
        fs::create_dir_all(dependency.parent().expect("namespace parent")).expect("create project");
        fs::write(
            &dependency,
            "namespace app; public int Value() { return 42; }",
        )
        .expect("write namespace dependency");
        let root = directory.join("main.aster");
        fs::write(
            &root,
            "using aster.math; using app; public class Program { public static int Main() { return Math.Max(0, Value()); } }",
        )
        .expect("write root");

        let initial = build_and_run(root.to_str().expect("UTF-8 path"), None);
        let dependencies = initial.dependencies.expect("successful dependency graph");
        assert!(
            dependencies
                .contains(&fs::canonicalize(&dependency).expect("canonical namespace dependency"))
        );

        fs::write(&dependency, "namespace app; public int Value( {")
            .expect("write invalid namespace dependency");
        let failed = build_and_run(root.to_str().expect("UTF-8 path"), None);
        assert!(!failed.succeeded);
        assert!(failed.dependencies.is_none());
        assert!(
            dependencies
                .contains(&fs::canonicalize(&dependency).expect("canonical namespace dependency"))
        );
        fs::remove_dir_all(directory).expect("remove test project");
    }
}
