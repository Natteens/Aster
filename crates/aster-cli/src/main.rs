mod doctor;
mod new_project;
mod stdlib_discovery;
mod watch;

use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CliExitCode {
    Success = 0,
    Failure = 1,
    Usage = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CliError {
    Failure,
    Usage,
}

type CliResult = Result<(), CliError>;

fn main() -> ExitCode {
    let code = match run(env::args().skip(1)) {
        Ok(()) => CliExitCode::Success,
        Err(CliError::Failure) => CliExitCode::Failure,
        Err(CliError::Usage) => CliExitCode::Usage,
    };
    ExitCode::from(code as u8)
}

fn run(mut arguments: impl Iterator<Item = String>) -> CliResult {
    match arguments.next().as_deref() {
        Some("new") => run_new_command(&mut arguments),
        Some("fetch") => run_fetch_command(&mut arguments),
        Some("doctor") => {
            reject_extra_argument(&mut arguments, "aster doctor")?;
            if doctor::run() {
                Ok(())
            } else {
                Err(CliError::Failure)
            }
        }
        Some(command @ ("check" | "dump-hir" | "dump-mir")) => {
            run_validation_command(command, &mut arguments)
        }
        Some("test") => run_test_command(&mut arguments),
        Some("run") => run_execute_command(&mut arguments),
        Some("watch") => run_watch_command(&mut arguments),
        Some("--version" | "-V") => {
            if let Some(argument) = arguments.next() {
                return Err(usage_error(
                    format!("unexpected argument `{argument}`"),
                    "aster --version",
                ));
            }
            println!("aster {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") => {
            if let Some(argument) = arguments.next() {
                return Err(usage_error(
                    format!("unexpected argument `{argument}`"),
                    "aster --help",
                ));
            }
            print_help();
            Ok(())
        }
        None => {
            print_help();
            Ok(())
        }
        Some(command) => {
            eprintln!("error: unknown command `{command}`\n\nRun `aster --help` for usage.");
            Err(CliError::Usage)
        }
    }
}

fn run_fetch_command(arguments: &mut impl Iterator<Item = String>) -> CliResult {
    const USAGE: &str = "aster fetch [--update <PACKAGE>]";
    let first = arguments.next();
    if matches!(first.as_deref(), Some("--help" | "-h")) {
        reject_extra_argument(arguments, USAGE)?;
        print_command_help("fetch");
        return Ok(());
    }
    let update = match first.as_deref() {
        None => None,
        Some("--update") => Some(
            arguments
                .next()
                .filter(|value| !value.is_empty() && !value.starts_with('-'))
                .ok_or_else(|| usage_error("missing package name after `--update`", USAGE))?,
        ),
        Some(argument) if argument.starts_with('-') => {
            return Err(usage_error(format!("unknown flag `{argument}`"), USAGE));
        }
        Some(argument) => {
            return Err(usage_error(
                format!("unexpected argument `{argument}`"),
                USAGE,
            ));
        }
    };
    reject_extra_argument(arguments, USAGE)?;
    let current_directory = env::current_dir().map_err(|error| {
        eprintln!("error: could not determine current directory: {error}");
        CliError::Failure
    })?;
    let Some(manifest) = aster_compiler::find_manifest_path_from_directory(&current_directory)
    else {
        eprintln!("error: no Aster.toml was found");
        return Err(CliError::Failure);
    };
    match aster_compiler::fetch_dependencies(&manifest, update.as_deref()) {
        Ok(summary) if summary.package_count == 0 => {
            println!("No Git dependencies to fetch.");
            Ok(())
        }
        Ok(summary) => {
            let action = if summary.lockfile_changed {
                "resolved"
            } else {
                "verified"
            };
            println!(
                "ASTER dependencies {action}\n\nGit packages: {}\nLockfile: {}",
                summary.package_count,
                summary
                    .lockfile_path
                    .as_deref()
                    .map_or_else(|| "none".to_owned(), |path| path.display().to_string())
            );
            Ok(())
        }
        Err(diagnostics) => {
            for diagnostic in diagnostics {
                eprintln!("{}", diagnostic.render());
            }
            Err(CliError::Failure)
        }
    }
}

fn run_new_command(arguments: &mut impl Iterator<Item = String>) -> CliResult {
    const USAGE: &str = "aster new <NAME>";
    let Some(name) = arguments.next() else {
        return Err(usage_error("missing project name", USAGE));
    };
    if matches!(name.as_str(), "--help" | "-h") {
        reject_extra_argument(arguments, USAGE)?;
        print_command_help("new");
        return Ok(());
    }
    if name.starts_with('-') {
        return Err(usage_error(format!("unknown flag `{name}`"), USAGE));
    }
    if let Some(argument) = arguments.next() {
        return Err(usage_error(
            format!("unexpected argument `{argument}`"),
            USAGE,
        ));
    }
    if let Err(error) = new_project::validate_name(&name) {
        return Err(usage_error(error, USAGE));
    }
    let current_directory = env::current_dir().map_err(|error| {
        eprintln!("error: could not determine current directory: {error}");
        CliError::Failure
    })?;
    let path = new_project::create(&current_directory, &name).map_err(|error| {
        eprintln!("error: {error}");
        CliError::Failure
    })?;
    println!(
        "ASTER project created\n\nName: {name}\nPath: {}\n\nNext steps:\n  cd {name}\n  aster check\n  aster run",
        path.display()
    );
    Ok(())
}

fn run_validation_command(
    command: &str,
    arguments: &mut impl Iterator<Item = String>,
) -> CliResult {
    let usage = format!("aster {command} [FILE]");
    let file_name = arguments.next();
    if matches!(file_name.as_deref(), Some("--help" | "-h")) {
        reject_extra_argument(arguments, &usage)?;
        print_command_help(command);
        return Ok(());
    }
    if let Some(argument) = arguments.next() {
        return Err(usage_error(
            format!("unexpected argument `{argument}`"),
            &usage,
        ));
    }
    if let Some(argument) = file_name.as_deref()
        && argument.starts_with('-')
    {
        return Err(usage_error(format!("unknown flag `{argument}`"), &usage));
    }
    let file_name = resolve_source_argument(file_name)?;
    let stdlib = stdlib_discovery::discover().map_err(|()| CliError::Failure)?;
    process_file(command, &file_name, &stdlib).map_err(|()| CliError::Failure)
}

fn run_execute_command(arguments: &mut impl Iterator<Item = String>) -> CliResult {
    const USAGE: &str = "aster run [FILE] [--function <NAME>] [--memory-stats]";
    let mut arguments = arguments.collect::<Vec<_>>();
    if matches!(arguments.first().map(String::as_str), Some("--help" | "-h")) {
        if let Some(argument) = arguments.get(1) {
            return Err(usage_error(
                format!("unexpected argument `{argument}`"),
                USAGE,
            ));
        }
        print_command_help("run");
        return Ok(());
    }
    let file_name = if arguments
        .first()
        .is_some_and(|argument| !argument.starts_with('-'))
    {
        Some(arguments.remove(0))
    } else {
        None
    };
    let (function_name, memory_stats) =
        parse_execution_options(&mut arguments.into_iter(), USAGE, true)?;
    let file_name = resolve_source_argument(file_name)?;
    let stdlib = stdlib_discovery::discover().map_err(|()| CliError::Failure)?;
    run_file(&file_name, function_name.as_deref(), memory_stats, &stdlib)
        .map_err(|()| CliError::Failure)
}

#[allow(clippy::too_many_lines)]
fn run_test_command(arguments: &mut impl Iterator<Item = String>) -> CliResult {
    const USAGE: &str = "aster test";
    if let Some(argument) = arguments.next() {
        if matches!(argument.as_str(), "--help" | "-h") {
            reject_extra_argument(arguments, USAGE)?;
            print_command_help("test");
            return Ok(());
        }
        return Err(usage_error(
            format!("unexpected argument `{argument}`"),
            USAGE,
        ));
    }
    let current_directory = env::current_dir().map_err(|error| {
        eprintln!("error: could not determine current directory: {error}");
        CliError::Failure
    })?;
    let Some(manifest) = aster_compiler::find_manifest_path_from_directory(&current_directory)
    else {
        eprintln!("error: no Aster.toml was found");
        return Err(CliError::Failure);
    };
    let root = manifest.parent().ok_or_else(|| {
        eprintln!("error: Aster.toml has no parent directory");
        CliError::Failure
    })?;
    let source = root.join("app").join("main.aster");
    if !source.is_file() {
        eprintln!(
            "error: `aster test` requires a root source file at `{}`",
            source.display()
        );
        return Err(CliError::Failure);
    }
    let stdlib = stdlib_discovery::discover().map_err(|()| CliError::Failure)?;
    let project = match aster_compiler::compile_project_for_tests_with_stdlib(&source, stdlib) {
        Ok(project) => project,
        Err(diagnostics) => {
            for diagnostic in diagnostics {
                eprintln!("{}", diagnostic.render());
            }
            return Err(CliError::Failure);
        }
    };
    for diagnostic in project_diagnostics(&project) {
        eprintln!("{}", diagnostic.render());
    }
    let symbols = project
        .tests()
        .iter()
        .map(|test| test.symbol)
        .collect::<Vec<_>>();
    let mut prepared = match aster_codegen_cranelift::PreparedTestProgram::prepare(
        &project.compilation.mir,
        &symbols,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("error: {error}");
            return Err(CliError::Failure);
        }
    };
    print_stdout_line(format_args!("running {} tests", symbols.len()))
        .map_err(|()| CliError::Failure)?;
    let mut passed = 0usize;
    let mut failed = 0usize;
    for test in project.tests() {
        let console = aster_codegen_cranelift::MemoryConsoleBackend::default();
        let result = prepared.invoke(test.symbol, Box::new(console.clone()));
        match result {
            Ok(()) => {
                passed += 1;
                print_stdout_line(format_args!("PASS {}", test.display_name))
                    .map_err(|()| CliError::Failure)?;
            }
            Err(error) => {
                failed += 1;
                print_stdout_line(format_args!("FAIL {}", test.display_name))
                    .map_err(|()| CliError::Failure)?;
                for line in error.message().lines() {
                    print_stdout_line(format_args!("  {line}")).map_err(|()| CliError::Failure)?;
                }
                let output = console.output();
                if !output.is_empty() {
                    print_stdout_line(format_args!("  output:")).map_err(|()| CliError::Failure)?;
                    for line in String::from_utf8_lossy(&output).lines() {
                        print_stdout_line(format_args!("    {line}"))
                            .map_err(|()| CliError::Failure)?;
                    }
                }
            }
        }
    }
    if failed == 0 {
        print_stdout_line(format_args!("\ntest result: ok. {passed} passed; 0 failed"))
            .map_err(|()| CliError::Failure)?;
        Ok(())
    } else {
        print_stdout_line(format_args!(
            "\ntest result: FAILED. {passed} passed; {failed} failed"
        ))
        .map_err(|()| CliError::Failure)?;
        Err(CliError::Failure)
    }
}

fn run_watch_command(arguments: &mut impl Iterator<Item = String>) -> CliResult {
    const USAGE: &str = "aster watch <FILE> [--function <NAME>]";
    let Some(file_name) = arguments.next() else {
        return Err(usage_error("missing source file", USAGE));
    };
    if matches!(file_name.as_str(), "--help" | "-h") {
        reject_extra_argument(arguments, USAGE)?;
        print_command_help("watch");
        return Ok(());
    }
    if file_name.starts_with('-') {
        return Err(usage_error(
            format!("missing source file; unexpected option `{file_name}`"),
            USAGE,
        ));
    }
    let (function_name, _) = parse_execution_options(arguments, USAGE, false)?;
    let stdlib = stdlib_discovery::discover().map_err(|()| CliError::Failure)?;
    watch::watch_file(&file_name, function_name.as_deref(), &stdlib).map_err(|()| CliError::Failure)
}

fn parse_execution_options(
    arguments: &mut impl Iterator<Item = String>,
    usage: &str,
    allow_memory_stats: bool,
) -> Result<(Option<String>, bool), CliError> {
    let mut function_name = None;
    let mut memory_stats = false;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--function" => {
                if function_name.is_some() {
                    return Err(usage_error(
                        "`--function` was specified more than once",
                        usage,
                    ));
                }
                let Some(name) = arguments.next() else {
                    return Err(usage_error(
                        "missing function name after `--function`",
                        usage,
                    ));
                };
                if name.starts_with('-') {
                    return Err(usage_error(
                        "missing function name after `--function`",
                        usage,
                    ));
                }
                function_name = Some(name);
            }
            "--memory-stats" if allow_memory_stats && !memory_stats => memory_stats = true,
            "--memory-stats" if allow_memory_stats => {
                return Err(usage_error(
                    "`--memory-stats` was specified more than once",
                    usage,
                ));
            }
            _ => {
                return Err(usage_error(
                    format!("unexpected argument `{option}`"),
                    usage,
                ));
            }
        }
    }
    Ok((function_name, memory_stats))
}

fn reject_extra_argument(arguments: &mut impl Iterator<Item = String>, usage: &str) -> CliResult {
    if let Some(argument) = arguments.next() {
        return Err(usage_error(
            format!("unexpected argument `{argument}`"),
            usage,
        ));
    }
    Ok(())
}

fn usage_error(message: impl std::fmt::Display, usage: &str) -> CliError {
    eprintln!("error: {message}\nusage: {usage}");
    CliError::Usage
}

fn resolve_source_argument(argument: Option<String>) -> Result<String, CliError> {
    if let Some(argument) = argument {
        return Ok(argument);
    }
    let current_directory = env::current_dir().map_err(|error| {
        eprintln!("error: could not determine current directory: {error}");
        CliError::Failure
    })?;
    let Some(manifest) = aster_compiler::find_manifest_path_from_directory(&current_directory)
    else {
        eprintln!("error: no source file was provided and no Aster.toml was found");
        return Err(CliError::Failure);
    };
    let project_root = manifest.parent().ok_or_else(|| {
        eprintln!("error: Aster.toml has no parent directory");
        CliError::Failure
    })?;
    path_to_utf8(project_root.join("app").join("main.aster")).map_err(|()| CliError::Failure)
}

fn path_to_utf8(path: PathBuf) -> Result<String, ()> {
    path.into_os_string().into_string().map_err(|_| {
        eprintln!("error: project source path is not valid UTF-8");
    })
}

pub(crate) fn read_source(file_name: &str) -> Result<String, ()> {
    let path = Path::new(file_name);
    validate_source_file(path)?;
    fs::read_to_string(path).map_err(|error| {
        eprintln!("error: could not read `{file_name}`: {error}");
    })
}

fn validate_source_file(path: &Path) -> Result<(), ()> {
    let display = path.display();
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            eprintln!("error: file not found: {display}");
        } else {
            eprintln!("error: could not access `{display}`: {error}");
        }
    })?;
    if metadata.is_dir() {
        eprintln!("error: expected an Aster source file, found directory: {display}");
        return Err(());
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("aster") {
        eprintln!("error: expected an Aster source file with the `.aster` extension: {display}");
        return Err(());
    }
    Ok(())
}

fn write_stdout_line(
    writer: &mut impl Write,
    arguments: std::fmt::Arguments<'_>,
) -> Result<(), ()> {
    match writeln!(writer, "{arguments}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => {
            eprintln!("error: could not write command output: {error}");
            Err(())
        }
    }
}

fn print_stdout_line(arguments: std::fmt::Arguments<'_>) -> Result<(), ()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    write_stdout_line(&mut stdout, arguments)
}

fn process_file(
    command: &str,
    file_name: &str,
    stdlib: &aster_compiler::StandardLibrary,
) -> Result<(), ()> {
    validate_source_file(Path::new(file_name))?;
    match aster_compiler::compile_project_with_stdlib(Path::new(file_name), stdlib.clone()) {
        Ok(project) => {
            let compilation = &project.compilation;
            for diagnostic in project_diagnostics(&project) {
                eprintln!("{}", diagnostic.render());
            }
            // A library package declares no `[application]`, so `check` must
            // not demand an entry point from it.
            if project.requires_application_entry()
                && let Err(diagnostics) =
                    aster_compiler::select_application_entry(&project, Path::new(file_name))
            {
                for diagnostic in diagnostics {
                    eprintln!("{}", diagnostic.render());
                }
                return Err(());
            }
            // `check`/`dump-hir`/`dump-mir` never execute the program, but
            // must still reject anything `run` would reject structurally
            // (e.g. console I/O reachable from a worker body) -- reusing the
            // same validator `execute*` runs, never a second call-graph
            // analysis.
            if let Err(error) = aster_codegen_cranelift::validate(&compilation.mir) {
                eprintln!("error: {error}");
                return Err(());
            }
            match command {
                "dump-hir" => print_stdout_line(format_args!("{}", compilation.hir))?,
                "dump-mir" => print_stdout_line(format_args!("{}", compilation.mir))?,
                _ => {
                    println!(
                        "checked `{file_name}`: {} declaration(s), {} token(s), {} file(s)",
                        compilation.module.items.len(),
                        compilation.tokens.len(),
                        project.sources.len()
                    );
                }
            }
            Ok(())
        }
        Err(diagnostics) => {
            for diagnostic in diagnostics {
                eprintln!("{}", diagnostic.render());
            }
            Err(())
        }
    }
}

fn run_file(
    file_name: &str,
    function_name: Option<&str>,
    memory_stats: bool,
    stdlib: &aster_compiler::StandardLibrary,
) -> Result<(), ()> {
    validate_source_file(Path::new(file_name))?;
    let project =
        match aster_compiler::compile_project_with_stdlib(Path::new(file_name), stdlib.clone()) {
            Ok(compilation) => compilation,
            Err(diagnostics) => {
                for diagnostic in diagnostics {
                    eprintln!("{}", diagnostic.render());
                }
                return Err(());
            }
        };
    for diagnostic in project_diagnostics(&project) {
        eprintln!("{}", diagnostic.render());
    }
    match execute_project(&project, Path::new(file_name), function_name) {
        Ok((value, _, stats)) => {
            if !matches!(value, aster_codegen_cranelift::ExecutionValue::Void) {
                println!("{value}");
            }
            if memory_stats {
                print_memory_stats(&stats);
            }
            Ok(())
        }
        Err(()) => Err(()),
    }
}

pub(crate) fn execute_project(
    project: &aster_compiler::ProjectCompilation,
    root_file: &Path,
    function_name: Option<&str>,
) -> Result<
    (
        aster_codegen_cranelift::ExecutionValue,
        String,
        aster_codegen_cranelift::MemoryStats,
    ),
    (),
> {
    if let Some(function_name) = function_name {
        let Some(symbol) = project.root_public_function_symbol(function_name) else {
            eprintln!(
                "error: entry function `{function_name}` must be declared `public` in the root namespace"
            );
            return Err(());
        };
        return aster_codegen_cranelift::execute_symbol_with_stats(
            &project.compilation.mir,
            symbol,
        )
        .map(|(value, stats)| (value, function_name.to_owned(), stats))
        .map_err(|error| eprintln!("error: {error}"));
    }

    let entry =
        aster_compiler::select_application_entry(project, root_file).map_err(|diagnostics| {
            for diagnostic in diagnostics {
                eprintln!("{}", diagnostic.render());
            }
        })?;
    aster_codegen_cranelift::execute_symbol_with_stats(&project.compilation.mir, entry.symbol)
        .map(|(value, stats)| (value, entry.display_name, stats))
        .map_err(|error| eprintln!("error: {error}"))
}

pub(crate) fn project_diagnostics(
    project: &aster_compiler::ProjectCompilation,
) -> Vec<aster_compiler::ProjectDiagnostic> {
    project
        .compilation
        .diagnostics
        .iter()
        .cloned()
        .map(|mut diagnostic| {
            let source = project
                .sources
                .iter()
                .filter(|source| diagnostic.span.start >= source.offset)
                .max_by_key(|source| source.offset)
                .expect("a compilation diagnostic belongs to a project source");
            diagnostic.span.start = diagnostic.span.start.saturating_sub(source.offset);
            diagnostic.span.end = diagnostic.span.end.saturating_sub(source.offset);
            aster_compiler::ProjectDiagnostic {
                path: source.path.clone(),
                source: source.source.clone(),
                diagnostic,
            }
        })
        .collect()
}

fn print_memory_stats(stats: &aster_codegen_cranelift::MemoryStats) {
    println!("memory:");
    println!("  allocations: {}", stats.total_allocations);
    println!("  objects: {}", stats.object_allocations);
    println!("  arrays: {}", stats.array_allocations);
    println!("  strings: {}", stats.string_allocations);
    println!("  requested: {} bytes", stats.requested_bytes);
    println!("  used: {} bytes", stats.used_bytes);
    println!("  reserved: {} bytes", stats.reserved_bytes);
    println!("  peak used: {} bytes", stats.peak_used_bytes);
    println!("  peak reserved: {} bytes", stats.peak_reserved_bytes);
}

fn print_help() {
    println!(
        "Aster compiler\n\nUsage:\n  aster <command> [arguments]\n\nCommands:\n  new <NAME>       Create a new ASTER project\n  fetch            Fetch and lock Git dependencies\n  doctor           Diagnose the ASTER installation and environment\n  check [FILE]     Check a project or source file\n  run [FILE]       Run a project or source file\n  test             Run root-package tests\n  watch <FILE>     Watch and rerun a source file\n  dump-hir [FILE]  Print HIR\n  dump-mir [FILE]  Print MIR\n\nOptions:\n  -h, --help\n  -V, --version"
    );
}

fn print_command_help(command: &str) {
    let (usage, description) = match command {
        "new" => (
            "aster new <NAME>",
            "Create a new ASTER project in a child of the current directory.",
        ),
        "fetch" => (
            "aster fetch [--update <PACKAGE>]",
            "Fetch public HTTPS Git dependencies and write Aster.lock.",
        ),
        "check" => (
            "aster check [FILE]",
            "Validate an Aster project or source file.",
        ),
        "dump-hir" => (
            "aster dump-hir [FILE]",
            "Validate and print typed HIR without executing.",
        ),
        "dump-mir" => (
            "aster dump-mir [FILE]",
            "Validate and print control-flow MIR without executing.",
        ),
        "test" => (
            "aster test",
            "Run parameterless `test void` functions from tests/.",
        ),
        "run" => (
            "aster run [FILE] [--function <NAME>] [--memory-stats]",
            "Run the application entry point or an explicitly selected function.",
        ),
        "watch" => (
            "aster watch <FILE> [--function <NAME>]",
            "Recompile and rerun when a loaded project file changes.",
        ),
        _ => unreachable!("help requested for a known command"),
    };
    println!("{description}\n\nUsage: {usage}");
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{self, Write},
        sync::atomic::{AtomicU64, Ordering},
    };

    use aster_compiler::StandardLibrary;

    use super::{process_file, run, run_file, write_stdout_line};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn embedded() -> StandardLibrary {
        StandardLibrary::embedded()
    }

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn command_output_treats_broken_pipe_as_success() {
        let mut writer = BrokenPipeWriter;
        assert!(write_stdout_line(&mut writer, format_args!("partial dump")).is_ok());
    }

    #[test]
    fn help_accepts_dump_hir_command() {
        assert!(run(["--help".to_owned()].into_iter()).is_ok());
    }

    #[test]
    fn dump_hir_validates_and_prints_without_execution() {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("aster-dump-hir-{}-{id}.aster", std::process::id()));
        fs::write(&path, "public int Value() { return 1; }").expect("write test source");
        let result = process_file(
            "dump-hir",
            path.to_str().expect("UTF-8 test path"),
            &embedded(),
        );
        fs::remove_file(path).expect("remove test source");
        assert!(result.is_ok());
    }

    #[test]
    fn dump_mir_validates_and_prints_without_execution() {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("aster-dump-mir-{}-{id}.aster", std::process::id()));
        fs::write(&path, "public int Value() { return 1; }").expect("write test source");
        let result = process_file(
            "dump-mir",
            path.to_str().expect("UTF-8 test path"),
            &embedded(),
        );
        fs::remove_file(path).expect("remove test source");
        assert!(result.is_ok());
    }

    #[test]
    fn check_command_remains_available() {
        let path = test_file("check", "public int Value() { return 1; }");
        let result = process_file(
            "check",
            path.to_str().expect("UTF-8 test path"),
            &embedded(),
        );
        fs::remove_file(path).expect("remove test source");
        assert!(result.is_ok());
    }

    #[test]
    fn check_does_not_require_main_but_validates_an_existing_manifest() {
        let library = test_file("library", "public int Value() { return 1; }");
        assert!(process_file("check", library.to_str().expect("UTF-8 path"), &embedded()).is_ok());
        fs::remove_file(library).expect("remove library source");

        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("aster-check-manifest-{}-{id}", std::process::id()));
        fs::create_dir_all(&directory).expect("create test directory");
        fs::write(directory.join("Aster.toml"), "[application\nentry = 1")
            .expect("write invalid manifest");
        let root = directory.join("main.aster");
        fs::write(&root, "public int Value() { return 1; }").expect("write source");
        assert!(process_file("check", root.to_str().expect("UTF-8 path"), &embedded()).is_err());
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn run_command_executes_an_explicit_function() {
        let path = test_file("run", "public int Calculate() { return 42; }");
        let result = run_file(
            path.to_str().expect("UTF-8 test path"),
            Some("Calculate"),
            false,
            &embedded(),
        );
        fs::remove_file(path).expect("remove test source");
        assert!(result.is_ok());
    }

    #[test]
    fn run_command_executes_conventional_main() {
        let path = test_file(
            "main",
            "public class Program { public static int Main() { return 42; } }",
        );
        let result = run_file(
            path.to_str().expect("UTF-8 test path"),
            None,
            false,
            &embedded(),
        );
        fs::remove_file(path).expect("remove test source");
        assert!(result.is_ok());
    }

    #[test]
    fn explicit_function_takes_precedence_over_an_invalid_manifest() {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("aster-override-{}-{id}", std::process::id()));
        fs::create_dir_all(&directory).expect("create test project");
        fs::write(directory.join("Aster.toml"), "not valid toml = [")
            .expect("write invalid manifest");
        let root = directory.join("main.aster");
        fs::write(&root, "public int Calculate() { return 42; }").expect("write test source");
        let result = run_file(
            root.to_str().expect("UTF-8 test path"),
            Some("Calculate"),
            false,
            &embedded(),
        );
        fs::remove_dir_all(directory).expect("remove test project");
        assert!(result.is_ok());
    }

    #[test]
    fn manifest_entry_executes_with_same_namespace_files() {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("aster-manifest-run-{}-{id}", std::process::id()));
        fs::create_dir_all(directory.join("app")).expect("create test project");
        fs::write(
            directory.join("Aster.toml"),
            "[package]\nname = \"manifest_run\"\n\n[application]\nentry = \"app.Program.Main\"\n",
        )
        .expect("write manifest");
        fs::write(
            directory.join("app/math.aster"),
            "namespace app; public int Answer() { return 42; }",
        )
        .expect("write namespace sibling");
        let root = directory.join("app/main.aster");
        fs::write(
            &root,
            "namespace app; public class Program { public static int Main() { return Answer(); } }",
        )
        .expect("write root source");
        let result = run_file(
            root.to_str().expect("UTF-8 test path"),
            None,
            false,
            &embedded(),
        );
        fs::remove_dir_all(directory).expect("remove test project");
        assert!(result.is_ok());
    }

    #[test]
    fn function_from_a_used_namespace_cannot_be_selected_as_the_entry() {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("aster-entry-{}-{id}", std::process::id()));
        fs::create_dir_all(directory.join("app")).expect("create test project");
        let root = directory.join("main.aster");
        fs::write(&root, "using app; public int Run() { return Double(2); }")
            .expect("write root source");
        fs::write(
            directory.join("app/math.aster"),
            "namespace app; public int Double(int value) { return value * 2; }",
        )
        .expect("write namespace source");
        let result = run_file(
            root.to_str().expect("UTF-8 test path"),
            Some("app::Double"),
            false,
            &embedded(),
        );
        fs::remove_dir_all(directory).expect("remove test project");
        assert!(result.is_err());
    }

    fn test_file(label: &str, source: &str) -> std::path::PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("aster-{label}-{}-{id}.aster", std::process::id()));
        fs::write(&path, source).expect("write test source");
        path
    }

    #[test]
    fn memory_stats_flag_does_not_change_result() {
        let path = test_file("stats-flag", "public int Run() { return 7; }");
        let without = run_file(
            path.to_str().expect("UTF-8"),
            Some("Run"),
            false,
            &embedded(),
        );
        let with = run_file(
            path.to_str().expect("UTF-8"),
            Some("Run"),
            true,
            &embedded(),
        );
        fs::remove_file(&path).expect("remove test file");
        assert!(without.is_ok());
        assert!(with.is_ok());
    }

    #[test]
    fn memory_stats_flag_accepted_for_class_program() {
        let path = test_file(
            "stats-class",
            "public class Program { public static int Main() { return 1; } }",
        );
        let result = run_file(path.to_str().expect("UTF-8"), None, true, &embedded());
        fs::remove_file(&path).expect("remove test file");
        assert!(result.is_ok());
    }
}
