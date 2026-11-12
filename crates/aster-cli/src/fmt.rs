//! `aster fmt [--check] [FILE...]`: the one-style source formatter (TOOL-001).

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use aster_syntax::format::{FormatError, format_source};

use crate::{CliError, CliResult, print_command_help, usage_error};

const USAGE: &str = "aster fmt [--check] [FILE...]";

pub(crate) fn run(arguments: &mut impl Iterator<Item = String>) -> CliResult {
    let mut check = false;
    let mut targets = Vec::new();
    for argument in arguments {
        match argument.as_str() {
            "--help" | "-h" => {
                print_command_help("fmt");
                return Ok(());
            }
            "--check" => check = true,
            flag if flag.starts_with('-') => {
                return Err(usage_error(format!("unknown flag `{flag}`"), USAGE));
            }
            _ => targets.push(PathBuf::from(argument)),
        }
    }
    if targets.is_empty() {
        targets.push(project_root()?);
    }
    let mut files = Vec::new();
    for target in &targets {
        if target.is_dir() {
            collect_sources(target, &mut files);
        } else if target.is_file() {
            files.push(target.clone());
        } else {
            eprintln!("error: `{}` is not a file or directory", target.display());
            return Err(CliError::Failure);
        }
    }
    files.sort();
    files.dedup();
    let mut failed = false;
    let mut would_change = 0usize;
    for file in &files {
        match format_file(file, check) {
            Ok(false) => {}
            Ok(true) => {
                would_change += 1;
                if check {
                    println!("{}", file.display());
                } else {
                    println!("formatted {}", file.display());
                }
            }
            Err(()) => failed = true,
        }
    }
    if failed || (check && would_change > 0) {
        if check && would_change > 0 {
            eprintln!("error: {would_change} file(s) would be reformatted");
        }
        return Err(CliError::Failure);
    }
    Ok(())
}

/// Format one file. Returns whether its content differs from the canonical
/// form (and was rewritten unless `check` is set).
fn format_file(path: &Path, check: bool) -> Result<bool, ()> {
    let source = fs::read_to_string(path).map_err(|error| {
        eprintln!("error: could not read `{}`: {error}", path.display());
    })?;
    let formatted = match format_source(&source) {
        Ok(formatted) => formatted,
        Err(FormatError::Lex(diagnostics)) => {
            for diagnostic in diagnostics {
                eprintln!(
                    "{}",
                    diagnostic.render(&path.display().to_string(), &source)
                );
            }
            return Err(());
        }
        Err(error) => {
            eprintln!("error: `{}`: {error}", path.display());
            return Err(());
        }
    };
    if formatted == source {
        return Ok(false);
    }
    if !check {
        fs::write(path, formatted).map_err(|error| {
            eprintln!("error: could not write `{}`: {error}", path.display());
        })?;
    }
    Ok(true)
}

fn project_root() -> Result<PathBuf, CliError> {
    let current_directory = env::current_dir().map_err(|error| {
        eprintln!("error: could not determine current directory: {error}");
        CliError::Failure
    })?;
    let Some(manifest) = aster_compiler::find_manifest_path_from_directory(&current_directory)
    else {
        eprintln!("error: no source file was provided and no Aster.toml was found");
        return Err(CliError::Failure);
    };
    manifest.parent().map(Path::to_path_buf).ok_or_else(|| {
        eprintln!("error: Aster.toml has no parent directory");
        CliError::Failure
    })
}

fn collect_sources(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                collect_sources(&path, files);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "aster")
        {
            files.push(path);
        }
    }
}
