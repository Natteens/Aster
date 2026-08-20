use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn global_help_version_and_usage_use_stable_streams_and_codes() {
    for arguments in [vec![], vec!["--help"], vec!["-h"]] {
        let output = aster(Path::new("."), arguments);
        assert_eq!(output.status.code(), Some(0));
        assert!(stderr(&output).is_empty());
        assert!(stdout(&output).contains("aster <command> [arguments]"));
    }
    for arguments in [vec!["--version"], vec!["-V"]] {
        let output = aster(Path::new("."), arguments);
        assert_eq!(output.status.code(), Some(0));
        assert!(stderr(&output).is_empty());
        assert_eq!(
            stdout(&output).trim(),
            format!("aster {}", env!("CARGO_PKG_VERSION"))
        );
    }

    for arguments in [
        vec!["unknown"],
        vec!["--help", "extra"],
        vec!["--version", "extra"],
    ] {
        let output = aster(Path::new("."), arguments);
        assert_usage(&output);
    }
}

#[test]
fn invalid_subcommand_arguments_consistently_exit_with_usage() {
    let directory = temporary_directory("usage matrix");
    let source = directory.join("main.aster");
    fs::write(
        &source,
        "public class Program { public static int Main() { return 0; } }",
    )
    .expect("write source");
    let source = source.to_str().expect("UTF-8 source");

    let cases = [
        vec!["new"],
        vec!["new", "One", "Two"],
        vec!["new", "nested/project"],
        vec!["new", "--unknown"],
        vec!["doctor", "extra"],
        vec!["fetch", "extra"],
        vec!["fetch", "--update"],
        vec!["fetch", "--update", "math", "extra"],
        vec!["check", source, "extra"],
        vec!["check", "--unknown"],
        vec!["run", source, "extra"],
        vec!["run", "--unknown"],
        vec!["run", source, "--function"],
        vec!["dump-hir", source, "extra"],
        vec!["dump-mir", "--unknown"],
        vec!["watch"],
        vec!["watch", source, "extra"],
        vec!["watch", "--function", "Main"],
        vec!["watch", source, "--memory-stats"],
    ];
    for arguments in cases {
        let output = aster(&directory, arguments.clone());
        assert_eq!(
            output.status.code(),
            Some(2),
            "{arguments:?}: stdout={:?} stderr={:?}",
            stdout(&output),
            stderr(&output)
        );
        assert!(stdout(&output).is_empty(), "{arguments:?}");
        assert!(stderr(&output).contains("usage:"), "{arguments:?}");
    }
    fs::remove_dir_all(directory).expect("remove usage directory");
}

#[test]
fn new_distinguishes_invalid_names_from_operational_failures() {
    let directory = temporary_directory("new classification");
    let invalid = aster(&directory, ["new", "bad/name"]);
    assert_usage(&invalid);

    fs::create_dir(directory.join("Existing")).expect("create existing destination");
    fs::write(directory.join("Existing/owned.txt"), "owned").expect("write owned content");
    let existing = aster(&directory, ["new", "Existing"]);
    assert_failure(&existing);
    assert_eq!(
        fs::read_to_string(directory.join("Existing/owned.txt")).expect("read owned content"),
        "owned"
    );

    let created = aster(&directory, ["new", "Healthy"]);
    assert_eq!(created.status.code(), Some(0));
    assert!(stderr(&created).is_empty());
    assert!(stdout(&created).contains("ASTER project created"));
    fs::remove_dir_all(directory).expect("remove new directory");
}

#[test]
fn check_and_dumps_publish_output_only_after_success() {
    let directory = temporary_directory("check dumps ü");
    let valid = directory.join("valid.aster");
    fs::write(
        &valid,
        "public class Program { public static int Main() { int value = 1 + 2; return value; } }",
    )
    .expect("write valid source");
    let valid = valid.to_str().expect("UTF-8 valid source");

    let check = aster(&directory, ["check", valid]);
    assert_eq!(check.status.code(), Some(0));
    assert!(stderr(&check).is_empty());
    assert!(stdout(&check).contains("checked"));

    for command in ["dump-hir", "dump-mir"] {
        let output = aster(&directory, [command, valid]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert!(stderr(&output).is_empty());
        assert!(!stdout(&output).is_empty());
        if command == "dump-mir" {
            assert!(!stdout(&output).contains("kind: Binary"));
            assert!(stdout(&output).contains("Integer("));
            assert!(stdout(&output).contains("\"3\""));
        }
    }

    let language = directory.join("language.aster");
    fs::write(
        &language,
        "public struct Pair { public int left; public int right; public int Sum() { return left + right; } } public class Tools { public Tools() {} public T Identity<T>(T value) { return value; } } public class Program { public static int Main() { Pair pair = Pair { left: 20, right: 22 }; return new Tools().Identity<int>(pair.Sum()); } }",
    )
    .expect("write complete language source");
    let language = language.to_str().expect("UTF-8 language source");
    for command in ["check", "dump-hir", "dump-mir"] {
        let output = aster(&directory, [command, language]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert!(stderr(&output).is_empty());
        assert!(!stdout(&output).is_empty());
    }
    let output = aster(&directory, ["run", language]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "42");

    let foreign = directory.join("foreign.aster");
    fs::write(
        &foreign,
        "using aster.io; public unsafe foreign int Native(); public class Program { public static int Main() { WriteLine(\"must not run\"); unsafe { return Native(); } } }",
    )
    .expect("write foreign source");
    let foreign = foreign.to_str().expect("UTF-8 foreign source");
    for command in ["check", "dump-hir", "dump-mir"] {
        let output = aster(&directory, [command, foreign]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert!(stderr(&output).is_empty());
        assert!(!stdout(&output).is_empty());
    }
    let output = aster(&directory, ["run", foreign]);
    assert_failure(&output);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("missing foreign binding"));
    assert_diagnostic_is_controlled(&output);

    let invalid = directory.join("invalid.aster");
    fs::write(&invalid, "public class Program {").expect("write invalid source");
    for command in ["check", "dump-hir", "dump-mir"] {
        let output = aster(
            &directory,
            [command, invalid.to_str().expect("UTF-8 invalid source")],
        );
        assert_failure(&output);
        assert!(
            stdout(&output).is_empty(),
            "{command} emitted a partial result"
        );
        assert_diagnostic_is_controlled(&output);
    }

    let decimal = directory.join("decimal.aster");
    fs::write(
        &decimal,
        "public class Program { public static decimal Main() { return 1.25m; } }",
    )
    .expect("write deferred decimal source");
    for command in ["check", "dump-hir", "dump-mir", "run"] {
        let output = aster(
            &directory,
            [command, decimal.to_str().expect("UTF-8 decimal source")],
        );
        assert_failure(&output);
        assert!(stdout(&output).is_empty(), "{command} emitted partial IR");
        assert!(
            stderr(&output).contains("`decimal` is reserved but not supported"),
            "unexpected {command} diagnostic: {}",
            stderr(&output)
        );
        assert_diagnostic_is_controlled(&output);
    }

    let missing = aster(&directory, ["check", "missing.aster"]);
    assert_failure(&missing);
    assert!(stderr(&missing).contains("file not found"));
    fs::remove_dir_all(directory).expect("remove check directory");
}

#[test]
fn run_keeps_main_value_on_stdout_and_reports_runtime_failure_on_stderr() {
    let directory = temporary_directory("run contract");
    let valid = directory.join("valid.aster");
    fs::write(
        &valid,
        "public class Program { public static int Main() { return 42; } }",
    )
    .expect("write valid source");
    let output = aster(
        &directory,
        ["run", valid.to_str().expect("UTF-8 valid source")],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "42");
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));

    let runtime_failure = directory.join("runtime.aster");
    fs::write(
        &runtime_failure,
        "public class Program { public static int Main() { int zero = 0; return 1 / zero; } }",
    )
    .expect("write runtime source");
    let output = aster(
        &directory,
        [
            "run",
            runtime_failure.to_str().expect("UTF-8 runtime source"),
        ],
    );
    assert_failure(&output);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("integer division by zero"));
    assert_diagnostic_is_controlled(&output);
    fs::remove_dir_all(directory).expect("remove run directory");
}

#[test]
fn project_manifest_entrypoint_and_frontend_failures_are_operational() {
    let directory = temporary_directory("diagnostics");
    let sources = [
        ("token.aster", "@"),
        ("parser.aster", "public class Program {"),
        (
            "symbol.aster",
            "public int Value() { return MissingValue; }",
        ),
        (
            "type.aster",
            "public int Value() { string text = 1; return 0; }",
        ),
        (
            "generic.aster",
            "public int Value() { List<int, int> values = new List<int, int>(); return 0; }",
        ),
    ];
    for (name, source) in sources {
        let path = directory.join(name);
        fs::write(&path, source).expect("write diagnostic source");
        let output = aster(
            &directory,
            ["check", path.to_str().expect("UTF-8 diagnostic source")],
        );
        assert_failure(&output);
        assert!(stdout(&output).is_empty());
        assert_diagnostic_is_controlled(&output);
    }

    let no_entry = directory.join("no-entry.aster");
    fs::write(&no_entry, "public int Value() { return 0; }").expect("write entry source");
    let output = aster(
        &directory,
        ["run", no_entry.to_str().expect("UTF-8 entry source")],
    );
    assert_failure(&output);
    assert!(stderr(&output).contains("entry"));

    let project = directory.join("project");
    fs::create_dir_all(project.join("app")).expect("create project");
    fs::write(project.join("Aster.toml"), "[application\n").expect("write invalid manifest");
    fs::write(
        project.join("app/main.aster"),
        "public class Program { public static int Main() { return 0; } }",
    )
    .expect("write project source");
    let output = aster(&project, ["check"]);
    assert_failure(&output);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("Aster.toml"));
    fs::remove_dir_all(directory).expect("remove diagnostics directory");
}

#[test]
fn invalid_standard_library_is_an_operational_failure_without_fallback() {
    let directory = temporary_directory("stdlib");
    let source = directory.join("main.aster");
    fs::write(
        &source,
        "public class Program { public static int Main() { return 0; } }",
    )
    .expect("write source");
    let output = command(&directory)
        .env("ASTER_STDLIB", directory.join("missing-stdlib"))
        .args(["check", source.to_str().expect("UTF-8 source")])
        .output()
        .expect("run with invalid stdlib");
    assert_failure(&output);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("ASTER_STDLIB"));
    fs::remove_dir_all(directory).expect("remove stdlib directory");
}

#[test]
fn watch_rejects_initial_failures_instead_of_entering_the_poll_loop() {
    let directory = temporary_directory("watch initial");
    let invalid = directory.join("invalid.aster");
    fs::write(&invalid, "public class Program {").expect("write invalid watch source");
    let output = aster(
        &directory,
        ["watch", invalid.to_str().expect("UTF-8 watch source")],
    );
    assert_failure(&output);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("compilation failed"));
    assert!(!stderr(&output).contains("panicked"));

    let missing = aster(&directory, ["watch", "missing.aster"]);
    assert_failure(&missing);
    assert!(stderr(&missing).contains("file not found"));
    fs::remove_dir_all(directory).expect("remove watch directory");
}

#[test]
fn test_command_runs_root_tests_in_order_and_frames_failing_output() {
    let directory = temporary_directory("test command");
    fs::create_dir_all(directory.join("app")).expect("create app");
    fs::create_dir_all(directory.join("tests")).expect("create tests");
    fs::write(
        directory.join("Aster.toml"),
        "[package]\nname = \"sample\"\n",
    )
    .expect("write manifest");
    fs::write(
        directory.join("app/main.aster"),
        "namespace app; public int Value() { return 42; }",
    )
    .expect("write app");
    fs::write(
        directory.join("tests/suite.aster"),
        "namespace tests; using aster.testing; using aster.io; \
         test void Zed() { Assert.True(true); } \
         test void Alpha() { Assert.Equal(42, 40 + 2); } \
         test void Broken() { Log.Error(\"captured log\"); WriteLine(\"captured output\"); Assert.Equal(1, 2); }",
    )
    .expect("write tests");
    let output = aster(&directory, ["test"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let output = stdout(&output);
    assert!(output.contains("running 3 tests"));
    let broken = output.find("FAIL sample.tests.Broken").unwrap();
    let alpha = output.find("PASS sample.tests.Alpha").unwrap();
    let zed = output.find("PASS sample.tests.Zed").unwrap();
    assert!(alpha < broken && broken < zed, "{output}");
    assert!(output.contains("captured output"));
    assert!(output.contains("[error] captured log"));
    assert!(output.contains("expected: 1"));
    assert!(output.contains("actual:   2"));
    assert!(output.contains("2 passed; 1 failed"));

    let help = aster(&directory, ["test", "--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout(&help).contains("aster test"));
    for arguments in [
        vec!["test", "extra"],
        vec!["test", "--filter", "Alpha"],
        vec!["test", "--unknown"],
    ] {
        assert_usage(&aster(&directory, arguments));
    }
    fs::remove_dir_all(directory).expect("remove test project");
}

#[test]
fn test_command_succeeds_for_a_library_package_with_no_tests_or_application_entry() {
    let directory = temporary_directory("empty test command");
    fs::create_dir_all(directory.join("app")).expect("create app");
    fs::write(
        directory.join("Aster.toml"),
        "[package]\nname = \"library_sample\"\n",
    )
    .expect("write manifest");
    fs::write(
        directory.join("app/main.aster"),
        "namespace app; public int Value() { return 42; }",
    )
    .expect("write library source");

    let output = aster(&directory, ["test"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    assert!(stdout(&output).contains("running 0 tests"));
    assert!(stdout(&output).contains("0 passed; 0 failed"));
    fs::remove_dir_all(directory).expect("remove empty test project");
}

#[test]
fn test_command_reports_test_source_compilation_failures_before_execution() {
    let directory = temporary_directory("test compilation failure");
    fs::create_dir_all(directory.join("app")).expect("create app");
    fs::create_dir_all(directory.join("tests")).expect("create tests");
    fs::write(
        directory.join("Aster.toml"),
        "[package]\nname = \"broken_tests\"\n",
    )
    .expect("write manifest");
    fs::write(
        directory.join("app/main.aster"),
        "namespace app; public int Value() { return 42; }",
    )
    .expect("write app");
    fs::write(
        directory.join("tests/broken.aster"),
        "namespace tests; using aster.testing; test void Broken() { Assert.True(1); }",
    )
    .expect("write broken test");

    let output = aster(&directory, ["test"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(stderr(&output).contains("bool"));
    fs::remove_dir_all(directory).expect("remove broken test project");
}

#[test]
fn ordinary_project_commands_do_not_load_test_sources() {
    let directory = temporary_directory("ordinary commands ignore tests");
    fs::create_dir_all(directory.join("app")).expect("create app");
    fs::create_dir_all(directory.join("tests")).expect("create tests");
    fs::write(
        directory.join("Aster.toml"),
        "[package]\nname = \"production_only\"\n",
    )
    .expect("write manifest");
    fs::write(
        directory.join("app/main.aster"),
        "public int Main() { return 7; }",
    )
    .expect("write app");
    fs::write(directory.join("tests/broken.aster"), "@").expect("write invalid test source");
    let source = directory.join("app/main.aster");
    let source = source.to_str().expect("UTF-8 source");

    for command in ["check", "dump-hir", "dump-mir"] {
        let output = aster(&directory, [command, source]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    }
    let run = aster(&directory, ["run", source, "--function", "Main"]);
    assert_eq!(run.status.code(), Some(0), "{}", stderr(&run));
    assert_eq!(stdout(&run).trim(), "7");

    let tests = aster(&directory, ["test"]);
    assert_eq!(tests.status.code(), Some(1));
    assert!(stdout(&tests).is_empty());
    assert!(!stderr(&tests).is_empty());
    fs::remove_dir_all(directory).expect("remove project");
}

#[test]
fn explicit_test_source_uses_the_normal_check_and_dump_parser() {
    let directory = temporary_directory("explicit test source");
    let source = directory.join("sample.aster");
    fs::write(&source, "test void ParsedNormally() { }").expect("write test source");
    let source = source.to_str().expect("UTF-8 source");

    for command in ["check", "dump-hir", "dump-mir"] {
        let output = aster(&directory, [command, source]);
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    }
    fs::remove_dir_all(directory).expect("remove explicit source directory");
}

#[test]
fn test_output_is_framed_and_passing_output_is_discarded() {
    let directory = temporary_directory("test output framing");
    fs::create_dir_all(directory.join("app")).expect("create app");
    fs::create_dir_all(directory.join("tests")).expect("create tests");
    fs::write(
        directory.join("Aster.toml"),
        "[package]\nname = \"output_sample\"\n",
    )
    .expect("write manifest");
    fs::write(
        directory.join("app/main.aster"),
        "namespace app; public int Value() { return 42; }",
    )
    .expect("write app");
    fs::write(
        directory.join("tests/output.aster"),
        "namespace tests; using aster.io; using aster.testing; \
         test void Passing() { WriteLine(\"hidden passing output\"); } \
         test void Failing() { WriteLine(\"PASS fake\\nFAIL fake\\ntest result: ok\"); Assert.Equal(\"expected\\nPASS fake\", \"actual\\nFAIL fake\"); }",
    )
    .expect("write tests");

    let output = aster(&directory, ["test"]);
    assert_eq!(output.status.code(), Some(1));
    let output = stdout(&output);
    assert!(!output.contains("hidden passing output"));
    assert!(output.lines().any(|line| line == "    PASS fake"));
    assert!(output.lines().any(|line| line == "    FAIL fake"));
    assert!(output.lines().any(|line| line == "  PASS fake"));
    assert!(output.lines().any(|line| line == "  FAIL fake"));
    assert!(!output.lines().any(|line| line == "PASS fake"));
    assert!(!output.lines().any(|line| line == "FAIL fake"));
    fs::remove_dir_all(directory).expect("remove project");
}

#[test]
fn test_command_supplies_eof_input_without_reading_the_host_terminal() {
    let directory = temporary_directory("test eof input");
    fs::create_dir_all(directory.join("app")).expect("create app");
    fs::create_dir_all(directory.join("tests")).expect("create tests");
    fs::write(
        directory.join("Aster.toml"),
        "[package]\nname = \"eof_sample\"\n",
    )
    .expect("write manifest");
    fs::write(
        directory.join("app/main.aster"),
        "namespace app; public int Value() { return 42; }",
    )
    .expect("write app");
    fs::write(
        directory.join("tests/eof.aster"),
        "namespace tests; using aster.io; using aster.testing; \
         test void ReadsEof() { switch (ReadLine()) { case Some(value): Assert.False(true); case None: Assert.True(true); } }",
    )
    .expect("write test");

    let output = aster(&directory, ["test"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("PASS eof_sample.tests.ReadsEof"));
    fs::remove_dir_all(directory).expect("remove project");
}

#[test]
fn test_command_resolves_every_v1_scalar_assertion_overload() {
    let directory = temporary_directory("test scalar assertions");
    fs::create_dir_all(directory.join("app")).expect("create app");
    fs::create_dir_all(directory.join("tests")).expect("create tests");
    fs::write(
        directory.join("Aster.toml"),
        "[package]\nname = \"assertions\"\n",
    )
    .expect("write manifest");
    fs::write(
        directory.join("app/main.aster"),
        "namespace app; public int Value() { return 42; }",
    )
    .expect("write app");
    fs::write(
        directory.join("tests/scalars.aster"),
        "namespace tests; using aster.testing; \
         test void ScalarOverloads() { \
             Assert.True(true); Assert.False(false); \
             Assert.Equal(true, true); Assert.Equal('é', 'é'); \
             sbyte signedByte = -128; byte unsignedByte = 255; \
             short signedShort = -32768; ushort unsignedShort = 65535; \
             int signedInt = -1; uint unsignedInt = 4000000000u; \
             long signedLong = -1l; ulong unsignedLong = 18446744073709551615ul; \
             float single = -0.0f; double doubleValue = -0.0d; \
             Assert.Equal(signedByte, signedByte); Assert.Equal(unsignedByte, unsignedByte); \
             Assert.Equal(signedShort, signedShort); Assert.Equal(unsignedShort, unsignedShort); \
             Assert.Equal(signedInt, signedInt); Assert.Equal(unsignedInt, unsignedInt); \
             Assert.Equal(signedLong, signedLong); Assert.Equal(unsignedLong, unsignedLong); \
             Assert.Equal(single, 0.0f); Assert.Equal(doubleValue, 0.0d); \
             Assert.Equal(\"value\", \"value\"); \
         }",
    )
    .expect("write scalar test");

    let output = aster(&directory, ["test"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("PASS assertions.tests.ScalarOverloads"));
    fs::remove_dir_all(directory).expect("remove project");
}

#[test]
fn test_command_uses_exact_float_assertion_equality() {
    let directory = temporary_directory("test float assertions");
    fs::create_dir_all(directory.join("app")).expect("create app");
    fs::create_dir_all(directory.join("tests")).expect("create tests");
    fs::write(
        directory.join("Aster.toml"),
        "[package]\nname = \"float_assertions\"\n",
    )
    .expect("write manifest");
    fs::write(
        directory.join("app/main.aster"),
        "namespace app; public int Value() { return 42; }",
    )
    .expect("write app");
    fs::write(
        directory.join("tests/floats.aster"),
        "namespace tests; using aster.testing; \
         test void NanIsNotEqualToItself() { double zero = 0.0d; double nan = zero / zero; Assert.Equal(nan, nan); }",
    )
    .expect("write float test");

    let output = aster(&directory, ["test"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("FAIL float_assertions.tests.NanIsNotEqualToItself"));
    fs::remove_dir_all(directory).expect("remove project");
}

fn command(current_directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aster"));
    command.current_dir(current_directory);
    command
}

fn aster<I, S>(current_directory: &Path, arguments: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    command(current_directory)
        .args(arguments)
        .output()
        .expect("run ASTER CLI")
}

fn assert_usage(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={:?} stderr={:?}",
        stdout(output),
        stderr(output)
    );
    assert!(stdout(output).is_empty());
    assert!(!stderr(output).is_empty());
}

fn assert_failure(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout={:?} stderr={:?}",
        stdout(output),
        stderr(output)
    );
    assert!(!stderr(output).is_empty());
}

fn assert_diagnostic_is_controlled(output: &Output) {
    let error = stderr(output);
    assert!(!error.contains("thread 'main' panicked"), "{error}");
    assert!(!error.contains("stack backtrace"), "{error}");
    assert!(!error.contains("RUST_BACKTRACE"), "{error}");
}

fn temporary_directory(label: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-cli-contract-{label}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr")
}
