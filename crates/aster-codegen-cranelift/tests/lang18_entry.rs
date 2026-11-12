//! LANG-018: `Main()` / `Main(string[] args)` entry points, standard error and
//! environment access, driven through the same entry-selection and execution
//! API the CLI uses.

use std::{
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use aster_codegen_cranelift::{ExecutionValue, execute_entry_with_arguments};
use aster_compiler::{compile_project, select_application_entry};
use aster_runtime::ConsoleBackend;

/// Console that records standard output and standard error separately.
#[derive(Clone, Default)]
struct SplitConsole {
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl ConsoleBackend for SplitConsole {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stdout.lock().expect("stdout").extend_from_slice(bytes);
        Ok(())
    }

    fn write_error(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.lock().expect("stderr").extend_from_slice(bytes);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn read_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

struct Outcome {
    value: ExecutionValue,
    stdout: String,
    stderr: String,
}

fn source_path() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("aster-lang18-{}-{id}.aster", std::process::id()))
}

fn run(source: &str, arguments: &[&str]) -> Outcome {
    let path = source_path();
    std::fs::write(&path, source).expect("write temporary source");
    let project = compile_project(&path).expect("source compiles");
    let entry = select_application_entry(&project, &path).expect("entry selected");
    std::fs::remove_file(&path).ok();
    let console = SplitConsole::default();
    let arguments = arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect::<Vec<_>>();
    let (value, _) = execute_entry_with_arguments(
        &project.compilation.mir,
        entry.symbol,
        &arguments,
        Some(Box::new(console.clone())),
    )
    .expect("entry executes");
    let text = |buffer: &Arc<Mutex<Vec<u8>>>| {
        String::from_utf8(buffer.lock().expect("buffer").clone()).expect("UTF-8 output")
    };
    Outcome {
        value,
        stdout: text(&console.stdout),
        stderr: text(&console.stderr),
    }
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

#[test]
fn main_with_arguments_receives_count_and_values_without_program_name() {
    let outcome = run(ECHO, &["alpha", "beta gamma", "", "--flag"]);
    assert_eq!(outcome.value, ExecutionValue::Int(4));
    assert_eq!(outcome.stdout, "alpha\nbeta gamma\n\n--flag\n");
    assert_eq!(outcome.stderr, "");
}

#[test]
fn main_with_arguments_receives_an_empty_array_when_none_are_given() {
    let outcome = run(ECHO, &[]);
    assert_eq!(outcome.value, ExecutionValue::Int(0));
    assert_eq!(outcome.stdout, "");
}

#[test]
fn unicode_arguments_survive_intact() {
    let outcome = run(ECHO, &["olá", "日本語", "\u{1F600}", "\u{FFFD}"]);
    assert_eq!(outcome.value, ExecutionValue::Int(4));
    assert_eq!(outcome.stdout, "olá\n日本語\n\u{1F600}\n\u{FFFD}\n");
}

#[test]
fn void_main_with_arguments_completes_with_void() {
    let outcome = run(
        "using aster.io;
public class Program
{
    public static void Main(string[] args) { WriteLine(args[0]); }
}",
        &["only"],
    );
    assert_eq!(outcome.value, ExecutionValue::Void);
    assert_eq!(outcome.stdout, "only\n");
}

#[test]
fn parameterless_main_ignores_supplied_arguments() {
    let outcome = run(
        "public class Program { public static int Main() { return 7; } }",
        &["ignored", "values"],
    );
    assert_eq!(outcome.value, ExecutionValue::Int(7));
}

#[test]
fn int_result_is_returned_unchanged_for_the_host_to_use_as_exit_status() {
    for status in [0, 1, 42, 255, 256, 1000, -1] {
        let outcome = run(
            &format!("public class Program {{ public static int Main() {{ return {status}; }} }}"),
            &[],
        );
        assert_eq!(outcome.value, ExecutionValue::Int(status));
    }
}

#[test]
fn write_error_goes_to_standard_error_only() {
    let outcome = run(
        "using aster.io;
public class Program
{
    public static void Main()
    {
        Write(\"out1\");
        WriteError(\"err1\");
        WriteErrorLine(\"err2\");
        WriteLine(\"out2\");
    }
}",
        &[],
    );
    assert_eq!(outcome.value, ExecutionValue::Void);
    assert_eq!(outcome.stdout, "out1out2\n");
    assert_eq!(outcome.stderr, "err1err2\n");
}

#[test]
fn environment_variable_is_some_when_defined_and_none_when_absent() {
    let outcome = run(
        "using aster.io;
using aster.core;
public class Program
{
    public static int Main()
    {
        int found = 0;
        switch (Environment.GetVariable(\"CARGO_MANIFEST_DIR\"))
        {
            case Some(value):
                if (value.Length > 0) { found = 1; }
            case None:
                found = 0;
        }
        int missing = 0;
        switch (Environment.GetVariable(\"ASTER_LANG18_SURELY_UNDEFINED_VARIABLE\"))
        {
            case Some(value):
                missing = 0;
            case None:
                missing = 1;
        }
        switch (Environment.GetVariable(\"\"))
        {
            case Some(value):
                missing = 0;
            case None:
                missing = missing + 0;
        }
        return found * 10 + missing;
    }
}",
        &[],
    );
    assert_eq!(outcome.value, ExecutionValue::Int(11));
}

#[test]
fn environment_value_matches_the_host_process_environment() {
    let expected = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest directory");
    let outcome = run(
        "using aster.io;
using aster.core;
public class Program
{
    public static void Main()
    {
        switch (Environment.GetVariable(\"CARGO_MANIFEST_DIR\"))
        {
            case Some(value): Write(value);
            case None: Write(\"<none>\");
        }
    }
}",
        &[],
    );
    assert_eq!(outcome.stdout, expected);
}
