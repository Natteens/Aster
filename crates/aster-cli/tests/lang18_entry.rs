//! LANG-018 through the real CLI binary: program arguments after `--`, the
//! `int` result as the process exit status, standard error, and environment.

use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct Program {
    directory: PathBuf,
    main: PathBuf,
}

impl Program {
    fn new(label: &str, source: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "aster-cli-lang18-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create temporary directory");
        let main = directory.join("main.aster");
        fs::write(&main, source).expect("write program");
        Self { directory, main }
    }

    fn run(&self, trailing: &[OsString]) -> Output {
        self.command(trailing).output().expect("run Aster binary")
    }

    fn command(&self, trailing: &[OsString]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aster"));
        command.arg("run").arg(&self.main).args(trailing);
        command
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).ok();
    }
}

fn os(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn after_separator(values: &[&str]) -> Vec<OsString> {
    let mut all = os(&["--"]);
    all.extend(os(values));
    all
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr")
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a status")
}

const ECHO: &str = "using aster.io;
public class Program
{
    public static int Main(string[] args)
    {
        for (int index = 0; index < args.Length; index++)
        {
            WriteLine(args[index]);
        }
        return args.Length;
    }
}";

fn int_main(status: &str) -> String {
    format!("public class Program {{ public static int Main() {{ return {status}; }} }}")
}

#[test]
fn arguments_after_the_separator_reach_main_and_exclude_the_program_name() {
    let program = Program::new("args", ECHO);
    let output = program.run(&after_separator(&["one", "two words", "--flag", "--"]));
    assert_eq!(
        stdout(&output),
        "one\ntwo words\n--flag\n--\n",
        "{}",
        stderr(&output)
    );
    assert_eq!(exit_code(&output), 4);
}

#[test]
fn main_receives_an_empty_array_without_a_separator_or_with_nothing_after_it() {
    let program = Program::new("no-args", ECHO);
    let none = program.run(&[]);
    assert_eq!(stdout(&none), "");
    assert_eq!(exit_code(&none), 0);
    let empty = program.run(&os(&["--"]));
    assert_eq!(stdout(&empty), "");
    assert_eq!(exit_code(&empty), 0);
}

#[test]
fn unicode_arguments_are_passed_unchanged() {
    let program = Program::new("unicode", ECHO);
    let output = program.run(&after_separator(&[
        "ol\u{e1}",
        "\u{65e5}\u{672c}\u{8a9e}",
        "\u{1f642}",
    ]));
    assert_eq!(
        stdout(&output),
        "ol\u{e1}\n\u{65e5}\u{672c}\u{8a9e}\n\u{1f642}\n",
        "{}",
        stderr(&output)
    );
    assert_eq!(exit_code(&output), 3);
}

#[cfg(any(unix, windows))]
fn invalid_unicode_argument() -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'a', 0xFF, b'b'])
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        // A lone high surrogate is not valid UTF-16.
        OsString::from_wide(&[u16::from(b'a'), 0xD800, u16::from(b'b')])
    }
}

#[cfg(any(unix, windows))]
#[test]
fn invalid_unicode_in_an_argument_is_replaced_with_u_fffd() {
    let program = Program::new("invalid", ECHO);
    let mut trailing = os(&["--"]);
    trailing.push(invalid_unicode_argument());
    trailing.push(OsString::from("ok"));
    let output = program.run(&trailing);
    assert_eq!(stdout(&output), "a\u{FFFD}b\nok\n", "{}", stderr(&output));
    assert_eq!(exit_code(&output), 2);
}

#[test]
fn arguments_without_the_separator_are_rejected_as_usage_errors() {
    let program = Program::new("no-separator", ECHO);
    let output = program.run(&os(&["stray"]));
    assert_eq!(exit_code(&output), 2);
    assert!(stderr(&output).contains("unexpected argument `stray`"));
    assert!(stderr(&output).contains("[-- ARGS...]"));
}

#[test]
fn run_options_before_the_separator_keep_working() {
    let program = Program::new("options", ECHO);
    let output = program.run(&os(&["--memory-stats", "--", "x"]));
    assert_eq!(exit_code(&output), 1);
    assert!(stdout(&output).starts_with("x\n"));
    assert!(stdout(&output).contains("memory:"));
}

#[test]
fn int_main_result_is_the_exit_status_and_is_not_printed() {
    for (status, expected) in [("0", 0), ("1", 1), ("42", 42)] {
        let program = Program::new("status", &int_main(status));
        let output = program.run(&[]);
        assert_eq!(exit_code(&output), expected, "{}", stderr(&output));
        assert_eq!(stdout(&output), "", "the result is no longer printed");
        assert_eq!(stderr(&output), "");
    }
}

#[test]
fn exit_status_is_passed_to_the_operating_system_unchanged() {
    let program = Program::new("wide", &int_main("300"));
    let output = program.run(&[]);
    // POSIX observes the low 8 bits (300 is 0x12C, whose low byte is 44); Windows the full value.
    let expected = if cfg!(windows) { 300 } else { 44 };
    assert_eq!(exit_code(&output), expected);
    assert_eq!(stdout(&output), "");
}

#[cfg(windows)]
#[test]
fn negative_status_reaches_windows_unchanged() {
    let program = Program::new("negative", &int_main("-1"));
    let output = program.run(&[]);
    assert_eq!(exit_code(&output), -1);
}

#[test]
fn void_main_exits_zero_with_only_program_output() {
    let program = Program::new(
        "void",
        "using aster.io; public class Program { public static void Main() { WriteLine(\"hi\"); } }",
    );
    let output = program.run(&[]);
    assert_eq!(exit_code(&output), 0);
    assert_eq!(stdout(&output), "hi\n");
}

#[test]
fn runtime_failure_exits_one_with_a_diagnostic_on_standard_error() {
    let program = Program::new(
        "failure",
        "public class Program { public static int Main() { int zero = 0; return 1 / zero; } }",
    );
    let output = program.run(&[]);
    assert_eq!(exit_code(&output), 1);
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).contains("error:"), "{}", stderr(&output));
}

#[test]
fn compile_failure_exits_one() {
    let program = Program::new(
        "compile-failure",
        "public class Program { public static int Main() { return missing; } }",
    );
    let output = program.run(&[]);
    assert_eq!(exit_code(&output), 1);
}

#[test]
fn explicit_function_still_prints_its_value_and_exits_zero() {
    let program = Program::new("function", "public int Calculate() { return 42; }");
    let output = program.run(&os(&["--function", "Calculate"]));
    assert_eq!(exit_code(&output), 0);
    assert_eq!(stdout(&output).trim(), "42");
}

#[test]
fn standard_error_is_a_separate_stream() {
    let program = Program::new(
        "stderr",
        "using aster.io;
public class Program
{
    public static int Main()
    {
        Write(\"out\");
        WriteError(\"err\");
        WriteErrorLine(\"!\");
        WriteLine(\"put\");
        return 3;
    }
}",
    );
    let output = program.run(&[]);
    assert_eq!(stdout(&output), "output\n", "{}", stderr(&output));
    assert_eq!(stderr(&output), "err!\n");
    assert_eq!(exit_code(&output), 3);
}

const ENVIRONMENT: &str = "using aster.io;
using aster.core;
public class Program
{
    public static int Main(string[] args)
    {
        switch (Environment.GetVariable(args[0]))
        {
            case Some(value): WriteLine(\"[\" + value + \"]\"); return 0;
            case None: WriteLine(\"absent\"); return 1;
        }
    }
}";

#[test]
fn environment_variable_present_empty_and_absent() {
    let program = Program::new("environment", ENVIRONMENT);
    let mut present = program.command(&after_separator(&["ASTER_LANG18_PRESENT"]));
    let present = present
        .env("ASTER_LANG18_PRESENT", "value 1")
        .output()
        .expect("run Aster binary");
    assert_eq!(stdout(&present), "[value 1]\n", "{}", stderr(&present));
    assert_eq!(exit_code(&present), 0);

    let mut empty = program.command(&after_separator(&["ASTER_LANG18_EMPTY"]));
    let empty = empty
        .env("ASTER_LANG18_EMPTY", "")
        .output()
        .expect("run Aster binary");
    assert_eq!(
        stdout(&empty),
        "[]\n",
        "a defined empty variable is Some(\"\")"
    );
    assert_eq!(exit_code(&empty), 0);

    let mut absent = program.command(&after_separator(&["ASTER_LANG18_ABSENT"]));
    let absent = absent
        .env_remove("ASTER_LANG18_ABSENT")
        .output()
        .expect("run Aster binary");
    assert_eq!(stdout(&absent), "absent\n");
    assert_eq!(exit_code(&absent), 1);
}

#[test]
fn invalid_main_signature_is_reported_without_running() {
    let program = Program::new(
        "bad-signature",
        "public class Program { public static int Main(int[] args) { return 0; } }",
    );
    let output = program.run(&[]);
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).contains("has parameters other than a single `string[] args`"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn two_eligible_mains_are_ambiguous() {
    let program = Program::new(
        "ambiguous",
        "public class Program { public static int Main() { return 0; } public static int Main(string[] args) { return 1; } }",
    );
    let output = program.run(&[]);
    assert_eq!(exit_code(&output), 1);
    assert!(
        stderr(&output).contains("more than one conventional application entry"),
        "{}",
        stderr(&output)
    );
}
