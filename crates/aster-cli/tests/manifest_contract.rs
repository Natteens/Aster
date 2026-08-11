use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn no_file_commands_find_the_nearest_manifest_from_nested_directories() {
    let root = temporary_directory("nested-project");
    let project = root.join("project");
    let nested = project.join("tools/deep");
    fs::create_dir_all(project.join("app")).expect("create app directory");
    fs::create_dir_all(&nested).expect("create nested directory");
    fs::write(
        project.join("Aster.toml"),
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    )
    .expect("write manifest");
    fs::write(
        project.join("app/main.aster"),
        "namespace app; public class Program { public static int Main() { return 42; } }",
    )
    .expect("write source");

    for command in ["check", "dump-hir", "dump-mir"] {
        let output = aster(&nested, [command]);
        assert!(
            output.status.success(),
            "{command}: stdout={:?} stderr={:?}",
            stdout(&output),
            stderr(&output)
        );
        assert!(stderr(&output).is_empty(), "{command}");
        assert!(!stdout(&output).is_empty(), "{command}");
    }

    let output = aster(&nested, ["run"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "42");
    assert!(stderr(&output).is_empty());

    fs::remove_dir_all(root).expect("remove nested project");
}

#[test]
fn the_nearest_manifest_wins_when_ancestors_have_manifests() {
    let root = temporary_directory("nearest-manifest");
    fs::write(
        root.join("Aster.toml"),
        "schema = 999\n\n[application]\nentry = \"outer.Program.Main\"\n",
    )
    .expect("write outer manifest");

    let project = root.join("inner");
    let nested = project.join("scratch/deep");
    fs::create_dir_all(project.join("app")).expect("create app directory");
    fs::create_dir_all(&nested).expect("create nested directory");
    fs::write(
        project.join("Aster.toml"),
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    )
    .expect("write inner manifest");
    fs::write(
        project.join("app/main.aster"),
        "namespace app; public class Program { public static int Main() { return 7; } }",
    )
    .expect("write source");

    let output = aster(&nested, ["run"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "7");
    assert!(stderr(&output).is_empty());

    fs::remove_dir_all(root).expect("remove nearest-manifest project");
}

#[test]
fn explicit_function_ignores_an_invalid_relevant_manifest() {
    let project = temporary_directory("function-override");
    fs::write(project.join("Aster.toml"), "schema = \"future\"\n").expect("write invalid manifest");
    let source = project.join("main.aster");
    fs::write(&source, "public int Calculate() { return 42; }").expect("write source");

    let output = aster(
        &project,
        [
            "run",
            source.to_str().expect("UTF-8 source path"),
            "--function",
            "Calculate",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "42");
    assert!(stderr(&output).is_empty());

    fs::remove_dir_all(project).expect("remove function override project");
}

#[test]
fn explicit_function_resolves_a_package_qualified_root_symbol() {
    let project = temporary_directory("package-function");
    fs::write(project.join("Aster.toml"), "[package]\nname = \"tool\"\n")
        .expect("write package manifest");
    let source = project.join("main.aster");
    fs::write(&source, "public int Calculate() { return 42; }").expect("write source");

    let output = aster(
        &project,
        [
            "run",
            source.to_str().expect("UTF-8 source path"),
            "--function",
            "Calculate",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "42");
    assert!(stderr(&output).is_empty());

    fs::remove_dir_all(project).expect("remove package function project");
}

#[test]
fn a_library_manifest_needs_no_application_table() {
    let project = temporary_directory("library-package");
    fs::create_dir_all(project.join("math")).expect("create library namespace");
    fs::write(project.join("Aster.toml"), "[package]\nname = \"math\"\n")
        .expect("write library manifest");
    let source = project.join("math/answer.aster");
    fs::write(
        &source,
        "namespace math; public int Answer() { return 42; }",
    )
    .expect("write library source");

    let output = aster(
        &project,
        ["check", source.to_str().expect("UTF-8 source path")],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).is_empty());

    let output = aster(
        &project,
        ["run", source.to_str().expect("UTF-8 source path")],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("does not define an `[application]` entry"));
    fs::remove_dir_all(project).expect("remove library project");
}

#[test]
fn removed_schema_fields_are_rejected_by_public_commands() {
    for schema in ["1", "2"] {
        let project = temporary_directory(&format!("removed-schema-{schema}"));
        fs::create_dir_all(project.join("app")).expect("create app directory");
        fs::write(
            project.join("Aster.toml"),
            format!(
                "schema = {schema}\n\n[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n"
            ),
        )
        .expect("write removed schema manifest");
        fs::write(
            project.join("app/main.aster"),
            "namespace app; public class Program { public static int Main() { return 42; } }",
        )
        .expect("write application source");

        for command in ["check", "run", "dump-hir", "dump-mir"] {
            let output = aster(&project, [command]);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{command}: {}",
                stderr(&output)
            );
            let error = stderr(&output);
            assert!(
                error.contains("Aster.toml no longer uses a `schema` field; remove it"),
                "{command}: {error}"
            );
            assert!(!error.contains("panicked"), "{command}: {error}");
        }
        fs::remove_dir_all(project).expect("remove removed-schema project");
    }
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
        "aster-manifest-contract-{label}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create temporary directory");
    path
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr")
}
