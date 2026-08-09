//! Typed `Aster.toml` loading and project-root discovery.
//!
//! This module is the compiler-side authority for the current manifest schema.
//! Downstream semantic, HIR, MIR, backend, and runtime layers never depend on
//! TOML or source paths.

use std::{
    fs,
    path::{Path, PathBuf},
};

pub const CURRENT_MANIFEST_SCHEMA: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectManifest {
    pub application: ApplicationManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApplicationManifest {
    pub entry: ManifestEntry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManifestEntry {
    pub namespace: String,
    pub class: String,
    pub method: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManifestDiagnostic {
    pub message: String,
    pub help: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoadedManifest {
    pub path: PathBuf,
    pub result: Result<ProjectManifest, ManifestDiagnostic>,
}

/// Find the nearest `Aster.toml`, beginning at the root source directory.
#[must_use]
pub fn find_manifest_path(root_file: &Path) -> Option<PathBuf> {
    let absolute = fs::canonicalize(root_file).ok()?;
    find_manifest_in_ancestors(absolute.parent()?)
}

/// Find the nearest `Aster.toml` from a working directory or one of its ancestors.
#[must_use]
pub fn find_manifest_path_from_directory(directory: &Path) -> Option<PathBuf> {
    let absolute = fs::canonicalize(directory).ok()?;
    if !absolute.is_dir() {
        return None;
    }
    find_manifest_in_ancestors(&absolute)
}

pub(crate) fn load_manifest(root_file: &Path) -> Option<LoadedManifest> {
    let path = find_manifest_path(root_file)?;
    let result = fs::read_to_string(&path)
        .map_err(|error| ManifestDiagnostic {
            message: format!("could not read Aster.toml: {error}"),
            help: "make sure `Aster.toml` is readable UTF-8 text".to_owned(),
        })
        .and_then(|source| parse_manifest(&source));
    Some(LoadedManifest { path, result })
}

fn find_manifest_in_ancestors(start: &Path) -> Option<PathBuf> {
    for directory in start.ancestors() {
        let candidate = directory.join("Aster.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn parse_manifest(source: &str) -> Result<ProjectManifest, ManifestDiagnostic> {
    let document = source.parse::<toml::Value>().map_err(|error| {
        manifest_error(
            format!("invalid Aster.toml: {error}"),
            "use `schema = 1` followed by `[application]` and `entry = \"namespace.Class.Main\"`",
        )
    })?;
    let table = document.as_table().ok_or_else(|| {
        manifest_error(
            "Aster.toml must contain a TOML table",
            "use `schema = 1` followed by an `[application]` table",
        )
    })?;

    match table.get("schema") {
        None => {}
        Some(toml::Value::Integer(value)) if *value == CURRENT_MANIFEST_SCHEMA => {}
        Some(toml::Value::Integer(value)) => {
            return Err(manifest_error(
                format!("unsupported Aster.toml schema `{value}`"),
                format!(
                    "this ASTER build supports schema `{CURRENT_MANIFEST_SCHEMA}`; update the toolchain or migrate the manifest"
                ),
            ));
        }
        Some(_) => {
            return Err(manifest_error(
                "Aster.toml `schema` must be an integer",
                format!("write `schema = {CURRENT_MANIFEST_SCHEMA}`"),
            ));
        }
    }

    if let Some(key) = table
        .keys()
        .find(|key| !matches!(key.as_str(), "schema" | "application"))
    {
        return Err(manifest_error(
            format!("unknown top-level Aster.toml field `{key}`"),
            "schema 1 supports only `schema` and `[application]`",
        ));
    }

    let application = table.get("application").ok_or_else(|| {
        manifest_error(
            "Aster.toml does not define an `[application]` table",
            "add `[application]` and `entry = \"app.Program.Main\"`",
        )
    })?;
    let application = application.as_table().ok_or_else(|| {
        manifest_error(
            "Aster.toml `application` must be a table",
            "use `[application]` followed by `entry = \"app.Program.Main\"`",
        )
    })?;

    if let Some(key) = application.keys().find(|key| key.as_str() != "entry") {
        return Err(manifest_error(
            format!("unknown Aster.toml application field `{key}`"),
            "schema 1 supports only `application.entry`",
        ));
    }

    let entry = application.get("entry").ok_or_else(|| {
        manifest_error(
            "Aster.toml does not define `application.entry`",
            "add `entry = \"app.Program.Main\"` below `[application]`",
        )
    })?;
    let entry = entry.as_str().ok_or_else(|| {
        manifest_error(
            "Aster.toml `application.entry` must be a string",
            "write a dotted entry such as `entry = \"app.Program.Main\"`",
        )
    })?;
    let parts = entry.split('.').collect::<Vec<_>>();
    if parts.len() < 3 || parts.iter().any(|part| !valid_identifier(part)) {
        return Err(manifest_error(
            format!("application entry `{entry}` has an invalid format"),
            "use a dotted `namespace.Class.Main` name such as `app.Program.Main`",
        ));
    }
    let method = parts[parts.len() - 1].to_owned();
    if method != "Main" {
        return Err(manifest_error(
            format!("application entry must end in `.Main`, but found `.{method}`"),
            "point `application.entry` at a public static `Main` method",
        ));
    }

    Ok(ProjectManifest {
        application: ApplicationManifest {
            entry: ManifestEntry {
                namespace: parts[..parts.len() - 2].join("."),
                class: parts[parts.len() - 2].to_owned(),
                method,
            },
        },
    })
}

fn valid_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some('_' | 'a'..='z' | 'A'..='Z'))
        && characters.all(|character| matches!(character, '_' | 'a'..='z' | 'A'..='Z' | '0'..='9'))
}

fn manifest_error(message: impl Into<String>, help: impl Into<String>) -> ManifestDiagnostic {
    ManifestDiagnostic {
        message: message.into(),
        help: help.into(),
    }
}
