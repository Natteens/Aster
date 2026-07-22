use std::{
    fs,
    io::Write as _,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

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
fn string_interpolation_runs_and_reports_syntax_errors_cleanly() {
    let directory = temporary_directory("interpolation");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        r#"public class Calculator {
    private int x = 1233;
    private int y = 1;
    public int Run() { return Sum(x, y); }
    private int Sum(int a, int b) { return a + b; }
}
public class Program {
    public static void Main() {
        Calculator calculator = new Calculator();
        int result = calculator.Run();
        Log($"Sum: {result}");
    }
}"#,
    )
    .expect("write interpolation program");
    let output = aster(["run", main.to_str().expect("UTF-8 temporary path")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "[log] Sum: 1234");

    let bad = directory.join("bad.aster");
    fs::write(
        &bad,
        r#"public class Program { public static void Main() { Log($"{value:00}"); } }"#,
    )
    .expect("write malformed interpolation program");
    let bad_output = aster(["check", bad.to_str().expect("UTF-8 temporary path")]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(!bad_output.status.success());
    assert!(stderr(&bad_output).contains("format specifier"));
}

#[test]
fn field_initializer_construction_runs_without_panicking() {
    let directory = temporary_directory("field-initializer-construction");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "public class Dependency { public int Get() { return 42; } } public class Service { private Dependency dependency = new Dependency(); public int Read() { return dependency.Get(); } } public class Program { public static int Main() { Service service = new Service(); return service.Read(); } }",
    )
    .expect("write field initializer program");
    let check_output = aster(["check", main.to_str().expect("UTF-8 temporary path")]);
    let run_output = aster(["run", main.to_str().expect("UTF-8 temporary path")]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(check_output.status.success(), "{}", stderr(&check_output));
    assert!(run_output.status.success(), "{}", stderr(&run_output));
    assert_eq!(stdout(&run_output).trim(), "42");
}

#[test]
fn invalid_field_initializer_construction_is_a_diagnostic_not_a_panic() {
    let directory = temporary_directory("invalid-field-initializer-construction");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "public class Service { private Missing dependency = new Missing(); public Service() {} } public class Program { public static int Main() { return 0; } }",
    )
    .expect("write invalid field initializer program");
    let output = aster(["check", main.to_str().expect("UTF-8 temporary path")]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("unknown type `Missing`"), "{error}");
    assert!(!error.contains("panicked at"), "{error}");
    assert!(!error.contains("stack backtrace"), "{error}");
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

#[test]
fn aster_io_uses_real_stdin_and_stdout_under_run() {
    let directory = temporary_directory("console-io");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "using aster.core;\nusing aster.io;\n\
         public void Main() {\n\
             Write(\"Name: \");\n\
             Option<string> name = ReadLine();\n\
             switch (name) { case Some(value): WriteLine(\"Hi, \" + value + \"!\"); case None: WriteLine(\"no input\"); }\n\
         }",
    )
    .expect("write console io program");
    let mut child = Command::new(env!("CARGO_BIN_EXE_aster"))
        .args([
            "run",
            main.to_str().expect("UTF-8 temporary path"),
            "--function",
            "Main",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn Aster binary");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"Ada\n")
        .expect("write to child stdin");
    let output = child.wait_with_output().expect("wait for Aster binary");
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Name: Hi, Ada!\n");
}

#[test]
fn aster_io_reports_none_on_immediate_eof() {
    let directory = temporary_directory("console-io-eof");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "using aster.core;\nusing aster.io;\n\
         public void Main() {\n\
             Option<string> name = ReadLine();\n\
             switch (name) { case Some(value): WriteLine(value); case None: WriteLine(\"eof\"); }\n\
         }",
    )
    .expect("write console io program");
    let mut child = Command::new(env!("CARGO_BIN_EXE_aster"))
        .args([
            "run",
            main.to_str().expect("UTF-8 temporary path"),
            "--function",
            "Main",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn Aster binary");
    // Dropping the piped stdin handle immediately (never written to) closes
    // it, so the child sees EOF right away instead of blocking.
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for Aster binary");
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "eof\n");
}

#[test]
fn check_never_touches_stdin_or_program_output() {
    let directory = temporary_directory("console-io-check");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "using aster.io;\npublic void Main() { WriteLine(\"should not run\"); }",
    )
    .expect("write console io program");
    let output = Command::new(env!("CARGO_BIN_EXE_aster"))
        .args(["check", main.to_str().expect("UTF-8 temporary path")])
        .stdin(Stdio::null())
        .output()
        .expect("run Aster binary");
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!stdout(&output).contains("should not run"));
}

#[test]
fn aster_check_rejects_console_io_reachable_from_a_task_run_body() {
    let directory = temporary_directory("check-worker-io");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "using aster.io;\n\
         public int Body() { WriteLine(\"from a worker\"); return 0; }\n\
         public int Main() { Task<int> task = Task.Run(Body); return task.Wait(); }",
    )
    .expect("write worker console io program");
    let check_output = aster(["check", main.to_str().expect("UTF-8 temporary path")]);
    let run_output = aster([
        "run",
        main.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Main",
    ]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(
        !check_output.status.success(),
        "`aster check` must reject console I/O reachable from a Task.Run body just like `aster run` does"
    );
    assert!(!run_output.status.success());
    assert!(
        stderr(&check_output).contains("Task.Run"),
        "{}",
        stderr(&check_output)
    );
}

/// The M1E mandatory integrated program (M1A search/substring, M1B1/M1B2
/// parsing, M1C `ToString`, M1D console I/O, all together), run through the
/// real CLI subprocess with piped stdin/captured stdout.
#[test]
fn integrated_m1_program_runs_via_aster_run_subprocess() {
    let directory = temporary_directory("m1-integrated");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "using aster.core;\nusing aster.io;\n\
         public int Main() {\n\
             Write(\"Input: \");\n\
             Option<string> maybeLine = ReadLine();\n\
             switch (maybeLine) { case Some(line): return Process(line); case None: return 1; }\n\
         }\n\
         public int Process(string line) {\n\
             if (!line.Contains(\":\")) { WriteLine(\"invalid\"); return 1; }\n\
             int separator = line.IndexOf(\":\");\n\
             string name = line.Substring(0, separator);\n\
             string valueText = line.Substring(separator + 1);\n\
             Option<double> parsed = valueText.TryParseDouble();\n\
             switch (parsed) { case Some(value): return PrintResult(name, value); case None: return 2; }\n\
         }\n\
         public int PrintResult(string name, double value) {\n\
             WriteLine($\"{name}: {value.ToString()}\");\n\
             return 0;\n\
         }",
    )
    .expect("write integrated M1 program");
    let mut child = Command::new(env!("CARGO_BIN_EXE_aster"))
        .args([
            "run",
            main.to_str().expect("UTF-8 temporary path"),
            "--function",
            "Main",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn Aster binary");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"count:7\n")
        .expect("write to child stdin");
    let output = child.wait_with_output().expect("wait for Aster binary");
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    // `Main` returns `int`, so the CLI also prints that value (here `0`,
    // `PrintResult`'s success code) as its own trailing line, after the
    // program's own console output.
    assert_eq!(stdout(&output), "Input: count: 7\n0\n");
    assert_eq!(output.status.code(), Some(0));
}

fn aster<const N: usize>(arguments: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aster"))
        .args(arguments)
        .output()
        .expect("run Aster binary")
}

fn temporary_directory(label: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("aster-cli-{label}-{}-{id}", std::process::id()));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr")
}
