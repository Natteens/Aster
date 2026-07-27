mod doctor;
mod new_project;
mod stdlib_discovery;
mod watch;

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

fn main() -> ExitCode {
    match run(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::FAILURE,
    }
}

fn run(mut arguments: impl Iterator<Item = String>) -> Result<(), ()> {
    match arguments.next().as_deref() {
        Some("new") => run_new_command(&mut arguments),
        Some("doctor") => {
            reject_extra_argument(&mut arguments)?;
            if doctor::run() { Ok(()) } else { Err(()) }
        }
        Some(command @ ("check" | "dump-hir" | "dump-mir")) => {
            run_validation_command(command, &mut arguments)
        }
        Some("run") => run_execute_command(&mut arguments),
        Some("watch") => run_watch_command(&mut arguments),
        Some("--version" | "-V") => {
            if let Some(argument) = arguments.next() {
                eprintln!("error: unexpected argument `{argument}`");
                return Err(());
            }
            println!("aster {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h") => {
            if let Some(argument) = arguments.next() {
                eprintln!("error: unexpected argument `{argument}`");
                return Err(());
            }
            print_help();
            Ok(())
        }
        None => {
            print_help();
            Ok(())
        }
        Some(command) => {
            eprintln!("error: unknown command `{command}`\n");
            print_help();
            Err(())
        }
    }
}

fn run_new_command(arguments: &mut impl Iterator<Item = String>) -> Result<(), ()> {
    let Some(name) = arguments.next() else {
        eprintln!("error: missing project name\n\nUsage: aster new <NAME>");
        return Err(());
    };
    if matches!(name.as_str(), "--help" | "-h") {
        reject_extra_argument(arguments)?;
        print_command_help("new");
        return Ok(());
    }
    if let Some(argument) = arguments.next() {
        eprintln!("error: unexpected argument `{argument}`\n\nUsage: aster new <NAME>");
        return Err(());
    }
    let current_directory = env::current_dir().map_err(|error| {
        eprintln!("error: could not determine current directory: {error}");
    })?;
    let path = new_project::create(&current_directory, &name).map_err(|error| {
        eprintln!("error: {error}");
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
) -> Result<(), ()> {
    let file_name = arguments.next();
    if matches!(file_name.as_deref(), Some("--help" | "-h")) {
        reject_extra_argument(arguments)?;
        print_command_help(command);
        return Ok(());
    }
    if let Some(argument) = arguments.next() {
        eprintln!("error: unexpected argument `{argument}`\n\nUsage: aster {command} [FILE]");
        return Err(());
    }
    let file_name = resolve_source_argument(command, file_name)?;
    let stdlib = stdlib_discovery::discover()?;
    process_file(command, &file_name, &stdlib)
}

fn run_execute_command(arguments: &mut impl Iterator<Item = String>) -> Result<(), ()> {
    let usage = "Usage: aster run [FILE] [--function <NAME>] [--memory-stats]";
    let file_name = arguments.next();
    if matches!(file_name.as_deref(), Some("--help" | "-h")) {
        reject_extra_argument(arguments)?;
        print_command_help("run");
        return Ok(());
    }
    let file_name = resolve_source_argument("run", file_name)?;
    let (function_name, memory_stats) = parse_execution_options(arguments, usage)?;
    let stdlib = stdlib_discovery::discover()?;
    run_file(&file_name, function_name.as_deref(), memory_stats, &stdlib)
}

fn run_watch_command(arguments: &mut impl Iterator<Item = String>) -> Result<(), ()> {
    let usage = "Usage: aster watch <FILE> [--function <NAME>] [--memory-stats]";
    let Some(file_name) = arguments.next() else {
        eprintln!("error: missing source file\n\n{usage}");
        return Err(());
    };
    if matches!(file_name.as_str(), "--help" | "-h") {
        reject_extra_argument(arguments)?;
        print_command_help("watch");
        return Ok(());
    }
    let (function_name, _) = parse_execution_options(arguments, usage)?;
    let stdlib = stdlib_discovery::discover()?;
    watch::watch_file(&file_name, function_name.as_deref(), &stdlib)
}

fn parse_execution_options(
    arguments: &mut impl Iterator<Item = String>,
    usage: &str,
) -> Result<(Option<String>, bool), ()> {
    let mut function_name = None;
    let mut memory_stats = false;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--function" => {
                let Some(name) = arguments.next() else {
                    eprintln!("error: missing function name after `--function`\n\n{usage}");
                    return Err(());
                };
                function_name = Some(name);
            }
            "--memory-stats" => memory_stats = true,
            _ => {
                eprintln!("error: unexpected argument `{option}`\n\n{usage}");
                return Err(());
            }
        }
    }
    Ok((function_name, memory_stats))
}

fn reject_extra_argument(arguments: &mut impl Iterator<Item = String>) -> Result<(), ()> {
    if let Some(argument) = arguments.next() {
        eprintln!("error: unexpected argument `{argument}`");
        return Err(());
    }
    Ok(())
}

fn resolve_source_argument(command: &str, argument: Option<String>) -> Result<String, ()> {
    if let Some(argument) = argument {
        return Ok(argument);
    }
    let current_directory = env::current_dir().map_err(|error| {
        eprintln!("error: could not determine current directory: {error}");
    })?;
    let manifest = current_directory.join("Aster.toml");
    let source = current_directory.join("app").join("main.aster");
    if manifest.is_file() {
        return path_to_utf8(source);
    }
    eprintln!(
        "error: missing source file and no Aster.toml was found in the current directory\n\nUsage: aster {command} [FILE]"
    );
    Err(())
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
            if aster_compiler::find_manifest_path(Path::new(file_name)).is_some()
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
                "dump-hir" => println!("{}", compilation.hir),
                "dump-mir" => println!("{}", compilation.mir),
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
        if !project.is_root_public_function(function_name) {
            eprintln!(
                "error: entry function `{function_name}` must be declared `public` in the root namespace"
            );
            return Err(());
        }
        return aster_codegen_cranelift::execute_with_stats(
            &project.compilation.mir,
            function_name,
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
        "Aster compiler\n\nUsage: aster <COMMAND>\n\nCommands:\n  new <NAME>                         Create a new ASTER project\n  doctor                             Diagnose the ASTER installation and environment\n  check [FILE]                       Validate an Aster project or source file\n  dump-hir [FILE]                    Validate and print typed HIR without executing\n  dump-mir [FILE]                    Validate and print control-flow MIR without executing\n  run [FILE] [--function <NAME>] [--memory-stats]\n                                      Run application Main or an explicitly selected function\n  watch <FILE> [--function <NAME>]   Recompile and rerun on each file change\n\nOptions:\n  -h, --help                         Print help\n  -V, --version                      Print version"
    );
}

fn print_command_help(command: &str) {
    let (usage, description) = match command {
        "new" => (
            "aster new <NAME>",
            "Create a new ASTER project in a child of the current directory.",
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
        sync::atomic::{AtomicU64, Ordering},
    };

    use aster_compiler::StandardLibrary;

    use super::{process_file, run, run_file};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn embedded() -> StandardLibrary {
        StandardLibrary::embedded()
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
            "[application]\nentry = \"app.Program.Main\"\n",
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
