use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn global_help_and_version_describe_the_installed_binary() {
    let help = aster(["--help"]);
    assert!(help.status.success());
    let help = stdout(&help);
    assert!(help.contains("aster <COMMAND>"));
    assert!(help.contains("run <FILE>"));
    assert!(help.contains("check <FILE>"));
    assert!(help.contains("watch <FILE>"));

    let version = aster(["--version"]);
    assert!(version.status.success());
    assert_eq!(
        stdout(&version).trim(),
        format!("aster {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn public_commands_have_focused_help() {
    for command in ["run", "check", "watch"] {
        let output = aster([command, "--help"]);
        assert!(output.status.success(), "{command} help should succeed");
        assert!(stdout(&output).contains(&format!("Usage: aster {command} <FILE>")));
    }
}

#[test]
fn missing_file_has_a_short_specific_error() {
    let output = aster(["run", "this-file-does-not-exist.aster"]);
    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("error: file not found: this-file-does-not-exist.aster"));
    assert!(!error.contains(":1:1"));
}

#[test]
fn directory_is_not_mistaken_for_a_project_root_source() {
    let directory = temporary_directory("directory-input");
    let output = aster(["check", directory.to_str().expect("UTF-8 temporary path")]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("expected an Aster source file, found directory"));
}

#[test]
fn conventional_main_and_explicit_function_remain_supported() {
    let directory = temporary_directory("entries");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "public class Program { public static int Main() { return 42; } }",
    )
    .expect("write conventional entry");
    let main_output = aster(["run", main.to_str().expect("UTF-8 temporary path")]);
    assert!(main_output.status.success());
    assert_eq!(stdout(&main_output).trim(), "42");

    let function = directory.join("function.aster");
    fs::write(&function, "public int Calculate() { return 42; }").expect("write explicit function");
    let function_output = aster([
        "run",
        function.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Calculate",
    ]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(function_output.status.success());
    assert_eq!(stdout(&function_output).trim(), "42");
}

#[test]
fn void_main_prints_only_program_logs_and_no_artificial_value() {
    let directory = temporary_directory("void-entry");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "public class Worker { private int value = 41; public int Next() { return Bump(value); } private int Bump(int current) { return current + 1; } } public class Program { public static void Main() { Worker worker = new Worker(); int result = worker.Next(); Log(\"done\"); } }",
    )
    .expect("write void entry");
    let output = aster(["run", main.to_str().expect("UTF-8 temporary path")]);

    let void_function = directory.join("function.aster");
    fs::write(&void_function, "public void Greet() { Log(\"hi\"); }").expect("write void function");
    let function_output = aster([
        "run",
        void_function.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Greet",
    ]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "[log] done");

    assert!(
        function_output.status.success(),
        "{}",
        stderr(&function_output)
    );
    assert_eq!(stdout(&function_output).trim(), "[log] hi");
}

#[test]
fn absolute_source_path_and_embedded_stdlib_work_outside_the_repository() {
    let directory = temporary_directory("outside-repository");
    let source = directory.join("main.aster");
    fs::write(
        &source,
        "using aster.math; public class Program { public static int Main() { return Math.Max(40, 42); } }",
    )
    .expect("write stdlib program");
    let output = Command::new(env!("CARGO_BIN_EXE_aster"))
        .current_dir(std::env::temp_dir())
        .args(["run", source.to_str().expect("UTF-8 temporary path")])
        .output()
        .expect("run Aster binary outside repository");
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "42");
}

fn aster<const N: usize>(arguments: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aster"))
        .args(arguments)
        .output()
        .expect("run Aster binary")
}

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("aster-cli-{label}-{nonce}"));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr")
}
