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
    assert!(help.contains("aster <command> [arguments]"));
    assert!(help.contains("new <NAME>"));
    assert!(help.contains("doctor"));
    assert!(help.contains("run [FILE]"));
    assert!(help.contains("check [FILE]"));
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
    for command in ["run", "check"] {
        let output = aster([command, "--help"]);
        assert!(output.status.success(), "{command} help should succeed");
        assert!(stdout(&output).contains(&format!("Usage: aster {command} [FILE]")));
    }
    let watch = aster(["watch", "--help"]);
    assert!(watch.status.success());
    assert!(stdout(&watch).contains("Usage: aster watch <FILE>"));
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

/// M2D: `aster run` uses the real filesystem backend automatically (no
/// injection point exists at the CLI level, unlike the in-memory backend
/// `aster-codegen-cranelift`'s own test suite injects). Every path here is a
/// unique file inside a fresh temporary directory, cleaned up even when the
/// assertion fails would still leave the directory itself removed by the
/// unconditional `fs::remove_dir_all` before any `assert!`.
#[test]
fn aster_run_reads_a_real_utf8_file_via_read_all_text() {
    let directory = temporary_directory("read-all-text");
    let input = directory.join("input.txt");
    fs::write(&input, "Olá, ASTER! 🙂").expect("write real input file");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        format!(
            "using aster.core;\nusing aster.io;\n\
             public int Main() {{\n\
                 switch (ReadAllText(\"{}\")) {{\n\
                     case Ok(text): return text.Length;\n\
                     case Error(e): return -1;\n\
                 }}\n\
             }}",
            input.to_str().unwrap().replace('\\', "\\\\")
        ),
    )
    .expect("write program");
    let output = aster([
        "run",
        main.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Main",
    ]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    // `string.Length` counts Unicode scalar values, not UTF-8 bytes.
    assert_eq!(stdout(&output).trim(), "13");
}

#[test]
fn aster_run_writes_a_real_file_creating_and_truncating_it() {
    let directory = temporary_directory("write-all-text");
    let output_path = directory.join("output.txt");
    fs::write(&output_path, "stale content that must be truncated").expect("seed old content");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        format!(
            "using aster.core;\nusing aster.io;\n\
             public int Main() {{\n\
                 switch (WriteAllText(\"{}\", \"new\")) {{\n\
                     case Ok(count): return count;\n\
                     case Error(e): return -1;\n\
                 }}\n\
             }}",
            output_path.to_str().unwrap().replace('\\', "\\\\")
        ),
    )
    .expect("write program");
    let output = aster([
        "run",
        main.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Main",
    ]);
    let final_content = fs::read_to_string(&output_path).expect("read back real file");
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "3");
    assert_eq!(final_content, "new");
}

#[test]
fn aster_run_read_all_text_reports_not_found_for_a_missing_real_file() {
    let directory = temporary_directory("read-missing");
    let missing = directory.join("does-not-exist.txt");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        format!(
            "using aster.core;\nusing aster.io;\n\
             public int Main() {{\n\
                 switch (ReadAllText(\"{}\")) {{\n\
                     case Ok(text): return -1;\n\
                     case Error(e): switch (e.Kind) {{ case NotFound: return 0; default: return -2; }}\n\
                 }}\n\
             }}",
            missing.to_str().unwrap().replace('\\', "\\\\")
        ),
    )
    .expect("write program");
    let output = aster([
        "run",
        main.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Main",
    ]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "0");
}

#[test]
fn aster_run_read_all_text_reports_not_file_for_a_real_directory() {
    let directory = temporary_directory("read-directory");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        format!(
            "using aster.core;\nusing aster.io;\n\
             public int Main() {{\n\
                 switch (ReadAllText(\"{}\")) {{\n\
                     case Ok(text): return -1;\n\
                     case Error(e): switch (e.Kind) {{ case NotFile: return 0; default: return -2; }}\n\
                 }}\n\
             }}",
            directory.to_str().unwrap().replace('\\', "\\\\")
        ),
    )
    .expect("write program");
    let output = aster([
        "run",
        main.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Main",
    ]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "0");
}

#[test]
fn aster_run_read_all_text_reports_invalid_utf8_for_a_real_file() {
    let directory = temporary_directory("read-invalid-utf8");
    let input = directory.join("invalid.txt");
    fs::write(&input, [0xFF_u8, 0xFE]).expect("write invalid UTF-8 file");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        format!(
            "using aster.core;\nusing aster.io;\n\
             public int Main() {{\n\
                 switch (ReadAllText(\"{}\")) {{\n\
                     case Ok(text): return -1;\n\
                     case Error(e): switch (e.Kind) {{ case InvalidUtf8: return 0; default: return -2; }}\n\
                 }}\n\
             }}",
            input.to_str().unwrap().replace('\\', "\\\\")
        ),
    )
    .expect("write program");
    let output = aster([
        "run",
        main.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Main",
    ]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "0");
}

#[test]
fn aster_check_rejects_file_io_reachable_from_a_parallel_for_body() {
    let directory = temporary_directory("check-worker-file-io");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "using aster.core;\nusing aster.io;\n\
         public void Body(int i) { ReadAllText(\"a.txt\"); }\n\
         public int Main() { Parallel.For(0, 4, Body); return 0; }",
    )
    .expect("write worker file io program");
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
        "`aster check` must reject file I/O reachable from a Parallel.For body just like `aster run` does"
    );
    assert!(!run_output.status.success());
    assert!(
        stderr(&check_output).contains("ReadAllText"),
        "{}",
        stderr(&check_output)
    );
}

#[test]
fn aster_run_lists_real_direct_files_in_ordinal_order_and_reuses_the_paths() {
    let directory = temporary_directory("list-files");
    let data = directory.join("data");
    fs::create_dir(&data).expect("create data directory");
    fs::write(data.join("b.txt"), "b").expect("write b");
    fs::write(data.join("a.txt"), "hello").expect("write a");
    fs::write(data.join("empty.txt"), "").expect("write empty");
    fs::write(data.join("non_text.bin"), [0_u8, 255]).expect("write binary");
    fs::create_dir(data.join("Sub")).expect("create nested directory");
    fs::write(data.join("Sub").join("nested.txt"), "nested").expect("write nested");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        format!(
            "using aster.core;\nusing aster.io;\n\
             public int Main() {{\n\
                 switch (ListFiles(\"{}\")) {{\n\
                     case Ok(files):\n\
                         if (files.Length != 4) {{ return -1; }}\n\
                        switch (ReadAllText(files[0])) {{ case Ok(text): return text.Length; case Error(e): return -2; }}\n\
                     case Error(e): return -3;\n\
                 }}\n\
             }}",
            data.to_str().unwrap().replace('\\', "\\\\")
        ),
    )
    .expect("write program");
    let output = aster([
        "run",
        main.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Main",
    ]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "5");
}

/// Closing proof for M2 against the real host backend: the ASTER program
/// enumerates only direct files, recovers an invalid UTF-8 input through
/// `Result`, and writes its own deterministic summary through `WriteAllText`.
#[test]
fn aster_run_executes_the_filesystem_indexer_end_to_end() {
    let directory = temporary_directory("filesystem-indexer");
    let input = directory.join("input");
    let output_directory = directory.join("output");
    fs::create_dir(&input).expect("create input directory");
    fs::create_dir(&output_directory).expect("create output directory");
    fs::write(input.join("b.txt"), "β").expect("write b");
    fs::write(input.join("a.txt"), "alfa").expect("write a");
    fs::write(input.join("empty.txt"), "").expect("write empty");
    fs::write(input.join("binary.dat"), [0xff_u8, 0xfe]).expect("write binary");
    fs::create_dir(input.join("nested")).expect("create nested directory");
    fs::write(input.join("nested").join("ignored.txt"), "ignored").expect("write nested file");

    let main = directory.join("main.aster");
    let input = input
        .to_str()
        .expect("UTF-8 temporary path")
        .replace('\\', "\\\\");
    let output = output_directory
        .to_str()
        .expect("UTF-8 temporary path")
        .replace('\\', "\\\\");
    fs::write(
        &main,
        format!(
            "using aster.core;\nusing aster.io;\n\
             public Result<int, IOError> Index() {{\n\
                 string[] files = ListFiles(\"{input}\")?;\n\
                 if (files.Length != 4 || !files[0].EndsWith(\"a.txt\") || !files[1].EndsWith(\"b.txt\") || !files[2].EndsWith(\"binary.dat\") || !files[3].EndsWith(\"empty.txt\")) {{\n\
                     return Result<int, IOError>.Error(IOError {{ Kind: IOErrorKind.Other, OsCode: 0 }});\n\
                 }}\n\
                 int readable = 0;\n\
                 int invalid = 0;\n\
                 int characters = 0;\n\
                 for (int i = 0; i < files.Length; i++) {{\n\
                     switch (ReadAllText(files[i])) {{\n\
                         case Ok(text): readable = readable + 1; characters = characters + text.Length;\n\
                         case Error(error): switch (error.Kind) {{\n\
                             case InvalidUtf8: invalid = invalid + 1;\n\
                             default: return Result<int, IOError>.Error(error);\n\
                         }}\n\
                     }}\n\
                 }}\n\
                 string summaryPath = CombinePath(\"{output}\", \"summary.txt\")?;\n\
                 string summary = \"readable=\" + readable.ToString() + \";invalid=\" + invalid.ToString() + \";chars=\" + characters.ToString();\n\
                 int written = WriteAllText(summaryPath, summary)?;\n\
                 if (written != summary.Length) {{ return Result<int, IOError>.Error(IOError {{ Kind: IOErrorKind.Other, OsCode: 0 }}); }}\n\
                 return Result<int, IOError>.Ok(readable * 100 + invalid * 10 + characters);\n\
             }}\n\
             public int Main() {{ switch (Index()) {{ case Ok(value): return value; case Error(error): return -1; }} }}"
        ),
    )
    .expect("write indexer program");

    let execution = aster([
        "run",
        main.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Main",
    ]);
    let summary = fs::read_to_string(output_directory.join("summary.txt")).expect("read summary");
    fs::remove_dir_all(&directory).expect("remove temporary directory");

    assert!(execution.status.success(), "{}", stderr(&execution));
    assert_eq!(execution.status.code(), Some(0));
    assert_eq!(stdout(&execution).trim(), "315");
    assert_eq!(summary, "readable=3;invalid=1;chars=5");
}

#[test]
fn aster_check_and_run_both_reject_list_files_in_a_worker() {
    let directory = temporary_directory("check-worker-list-files");
    let main = directory.join("main.aster");
    fs::write(
        &main,
        "using aster.core;\nusing aster.io;\n\
         public void Helper(int i) { ListFiles(\"data\"); }\n\
         public void Body(int i) { Helper(i); }\n\
         public int Main() { Parallel.For(0, 4, Body); return 0; }",
    )
    .expect("write worker ListFiles program");
    let check_output = aster(["check", main.to_str().expect("UTF-8 temporary path")]);
    let hir_dump = aster(["dump-hir", main.to_str().expect("UTF-8 temporary path")]);
    let mir_dump = aster(["dump-mir", main.to_str().expect("UTF-8 temporary path")]);
    let run_output = aster([
        "run",
        main.to_str().expect("UTF-8 temporary path"),
        "--function",
        "Main",
    ]);
    fs::remove_dir_all(&directory).expect("remove temporary directory");
    assert!(!check_output.status.success());
    assert!(!hir_dump.status.success());
    assert!(!mir_dump.status.success());
    assert!(!run_output.status.success());
    assert!(
        stderr(&check_output).contains("ListFiles"),
        "{}",
        stderr(&check_output)
    );
    assert!(stderr(&hir_dump).contains("ListFiles"));
    assert!(stderr(&mir_dump).contains("ListFiles"));
}

/// M3F: the mandatory integrated program that exercises all three `foreach`
/// paths (array, string, List) with the M2 filesystem APIs, run through a real
/// `aster run` subprocess on a real temporary directory.
///
/// File structure mirroring the in-memory suite in `foreach_m3f_integration.rs`:
/// ```
/// input/a.txt          "alpha"       5 scalars
/// input/b.txt          "beta"        4 scalars
/// input/empty.txt      ""            0 scalars
/// input/invalid.bin    [0xFF 0xFE]   skipped (InvalidUtf8)
/// input/unicode.txt    "αβγ"         3 scalars (each 2 UTF-8 bytes)
/// input/nested/        subdirectory  non-recursive → not listed
/// output/              (written by the program)
/// ```
///
/// Expected: counts=[5,4,0,3], total=12, result.txt="files=4;total=12"
#[test]
fn m3f_integrated_program_runs_via_aster_run_subprocess_on_real_filesystem() {
    let directory = temporary_directory("m3f-integrated");

    // Build the directory structure.
    let input = directory.join("input");
    let output = directory.join("output");
    fs::create_dir_all(&input).expect("create input directory");
    fs::create_dir_all(&output).expect("create output directory");
    fs::create_dir_all(input.join("nested")).expect("create nested subdirectory");
    fs::write(input.join("a.txt"), "alpha").expect("write a.txt");
    fs::write(input.join("b.txt"), "beta").expect("write b.txt");
    fs::write(input.join("empty.txt"), "").expect("write empty.txt");
    fs::write(input.join("invalid.bin"), [0xFF_u8, 0xFE]).expect("write invalid.bin");
    fs::write(input.join("unicode.txt"), "\u{03B1}\u{03B2}\u{03B3}").expect("write unicode.txt");
    fs::write(input.join("nested").join("ignored.txt"), "ignored").expect("write ignored.txt");

    // Write the ASTER program. It uses relative paths so the subprocess
    // working directory must be `directory`.
    let main = directory.join("m3f_main.aster");
    fs::write(
        &main,
        "using aster.core;\nusing aster.io;\n\
         public Result<int, IOError> Run(string inputDir, string outputDir) {\n\
             string[] files = ListFiles(inputDir)?;\n\
             List<int> counts = new List<int>();\n\
             foreach (string file in files) {\n\
                 switch (ReadAllText(file)) {\n\
                     case Ok(text):\n\
                         int count = 0;\n\
                         foreach (char c in text) { count = count + 1; }\n\
                         counts.Add(count);\n\
                     case Error(error):\n\
                         bool skip = error.Kind == IOErrorKind.InvalidUtf8;\n\
                         if (!skip) { return Result<int, IOError>.Error(error); }\n\
                 }\n\
             }\n\
             int total = 0;\n\
             foreach (int count in counts) { total = total + count; }\n\
             string resultPath = CombinePath(outputDir, \"result.txt\")?;\n\
             string summary = \"files=\" + counts.Length.ToString() + \";total=\" + total.ToString();\n\
             int written = WriteAllText(resultPath, summary)?;\n\
             return Result<int, IOError>.Ok(total);\n\
         }\n\
         public int Main() {\n\
             switch (Run(\"input\", \"output\")) {\n\
                 case Ok(value): return value;\n\
                 case Error(error): return -1;\n\
             }\n\
         }",
    )
    .expect("write M3F integrated program");

    // Run `aster run` with the temp directory as working directory so that
    // the relative paths "input" and "output" resolve correctly.
    let run_output = Command::new(env!("CARGO_BIN_EXE_aster"))
        .args([
            "run",
            main.to_str().expect("UTF-8 temporary path"),
            "--function",
            "Main",
        ])
        .current_dir(&directory)
        .output()
        .expect("spawn Aster binary");

    let result_path = output.join("result.txt");
    let written_summary = fs::read_to_string(&result_path).ok();
    fs::remove_dir_all(&directory).expect("remove temporary directory");

    // Verify process exit and printed return value.
    assert!(run_output.status.success(), "{}", stderr(&run_output));
    // `aster run` prints the int return value on a trailing line.
    assert_eq!(
        stdout(&run_output).trim(),
        "12",
        "returned total scalar count"
    );
    assert_eq!(run_output.status.code(), Some(0));

    // Verify the written file.
    assert_eq!(
        written_summary.as_deref(),
        Some("files=4;total=12"),
        "WriteAllText output must match the computed summary"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn m4f_dictionary_program_runs_via_cli_on_real_filesystem() {
    let directory = temporary_directory("m4f-dictionary");
    let input = directory.join("input");
    let output = directory.join("output");
    fs::create_dir_all(input.join("nested")).expect("create M4F input tree");
    fs::create_dir_all(&output).expect("create M4F output directory");
    fs::write(input.join("a.txt"), "alpha").expect("write a.txt");
    fs::write(input.join("b.txt"), "beta").expect("write b.txt");
    fs::write(input.join("empty.txt"), "").expect("write empty.txt");
    fs::write(input.join("invalid.bin"), [0xFF_u8, 0xFE]).expect("write invalid.bin");
    fs::write(input.join("unicode.txt"), "\u{03B1}\u{03B2}\u{03B3}").expect("write unicode.txt");
    fs::write(input.join("nested").join("ignored.txt"), "ignored").expect("write ignored.txt");

    let main = directory.join("m4f_main.aster");
    fs::write(
        &main,
        r#"
            using aster.core;
            using aster.io;
            using aster.collections;

            public Result<int, IOError> Analyze(string inputDir, string outputDir)
            {
                string[] files = ListFiles(inputDir)?;
                Dictionary<string, int> fileCounts = new Dictionary<string, int>();
                Dictionary<char, int> characterCounts = new Dictionary<char, int>();
                Dictionary<string, int> categories = new Dictionary<string, int>();
                List<int> scalarCounts = new List<int>();
                categories.Add("valid", 0);
                categories.Add("invalid", 0);

                foreach (string file in files)
                {
                    switch (ReadAllText(file))
                    {
                        case Ok(text):
                            int count = 0;
                            foreach (char scalar in text)
                            {
                                count = count + 1;
                                switch (characterCounts.TryGet(scalar))
                                {
                                    case Some(current):
                                        characterCounts.Set(scalar, current + 1);
                                    case None:
                                        characterCounts.Add(scalar, 1);
                                }
                            }
                            fileCounts.Add(file, count);
                            scalarCounts.Add(count);
                            switch (categories.TryGet("valid"))
                            {
                                case Some(current):
                                    categories.Set("valid", current + 1);
                                case None:
                                    return Result<int, IOError>.Ok(-10);
                            }
                        case Error(error):
                            if (error.Kind != IOErrorKind.InvalidUtf8)
                            {
                                return Result<int, IOError>.Error(error);
                            }
                            switch (categories.TryGet("invalid"))
                            {
                                case Some(current):
                                    categories.Set("invalid", current + 1);
                                case None:
                                    return Result<int, IOError>.Ok(-11);
                            }
                    }
                }

                if (fileCounts.ContainsKey("input/nested/ignored.txt"))
                {
                    return Result<int, IOError>.Ok(-12);
                }
                DictionaryEntry<string, int>[] fileEntries = fileCounts.Entries();
                DictionaryEntry<char, int>[] characterEntries = characterCounts.Entries();
                categories.Remove("valid");
                categories.Add("valid", scalarCounts.Length);
                DictionaryEntry<string, int>[] categoryEntries = categories.Entries();

                int total = 0;
                foreach (DictionaryEntry<string, int> entry in fileEntries)
                {
                    total = total + entry.Value;
                }
                int listTotal = 0;
                foreach (int value in scalarCounts) { listTotal = listTotal + value; }
                if (total != listTotal) { return Result<int, IOError>.Ok(-13); }
                if (categoryEntries[0].Key != "invalid"
                    || categoryEntries[1].Key != "valid")
                {
                    return Result<int, IOError>.Ok(-14);
                }

                string resultPath = CombinePath(outputDir, "result.txt")?;
                string summary =
                    "files=" + fileEntries.Length.ToString()
                    + ";total=" + total.ToString()
                    + ";unique=" + characterEntries.Length.ToString()
                    + ";categories=" + categoryEntries[0].Key + "," + categoryEntries[1].Key;
                int written = WriteAllText(resultPath, summary)?;
                return Result<int, IOError>.Ok(
                    fileEntries.Length + total + characterEntries.Length
                    + categoryEntries.Length
                );
            }

            public int Main()
            {
                switch (Analyze("input", "output"))
                {
                    case Ok(value): return value;
                    case Error(error): return -1;
                }
            }
        "#,
    )
    .expect("write M4F program");

    let main_path = main.to_str().expect("UTF-8 temporary path");
    let check = Command::new(env!("CARGO_BIN_EXE_aster"))
        .args(["check", main_path])
        .current_dir(&directory)
        .output()
        .expect("run aster check");
    let hir = Command::new(env!("CARGO_BIN_EXE_aster"))
        .args(["dump-hir", main_path])
        .current_dir(&directory)
        .output()
        .expect("run aster dump-hir");
    let mir = Command::new(env!("CARGO_BIN_EXE_aster"))
        .args(["dump-mir", main_path])
        .current_dir(&directory)
        .output()
        .expect("run aster dump-mir");
    let run = Command::new(env!("CARGO_BIN_EXE_aster"))
        .args(["run", main_path, "--function", "Main"])
        .current_dir(&directory)
        .output()
        .expect("run M4F program");

    let written = fs::read_to_string(output.join("result.txt")).ok();
    fs::remove_dir_all(&directory).expect("remove M4F temporary directory");

    assert!(check.status.success(), "{}", stderr(&check));
    assert!(hir.status.success(), "{}", stderr(&hir));
    assert!(mir.status.success(), "{}", stderr(&mir));
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run).trim(), "28", "unexpected summary: {written:?}");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(
        written.as_deref(),
        Some("files=4;total=12;unique=10;categories=invalid,valid")
    );
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
