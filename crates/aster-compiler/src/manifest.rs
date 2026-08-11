//! Typed `Aster.toml` loading and project-root discovery.
//!
//! This module is the compiler-side authority for every supported manifest
//! schema. Downstream semantic, HIR, MIR, backend, and runtime layers never
//! depend on TOML or source paths.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// The schema written by `aster new`. Older supported schemas keep their own
/// documented interpretation; see `docs/reference/compatibility.md`.
pub const CURRENT_MANIFEST_SCHEMA: i64 = 2;

/// Every schema this build understands. A manifest outside this set is
/// rejected rather than guessed.
const SUPPORTED_SCHEMAS: [i64; 2] = [1, 2];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectManifest {
    pub schema: i64,
    /// Declared package identity. Always present from schema 2; `None` for
    /// schema 1, which predates package identity.
    pub package: Option<PackageManifest>,
    /// Present when the package can be executed. Optional from schema 2, so a
    /// package can be a pure library.
    pub application: Option<ApplicationManifest>,
    /// Direct path dependencies, sorted by declared name.
    pub dependencies: Vec<DependencyManifest>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackageManifest {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DependencyManifest {
    /// The name the dependency is declared under. It must equal the declared
    /// `[package] name` of the manifest it resolves to.
    pub name: String,
    /// Raw path text, interpreted relative to the declaring manifest.
    pub path: String,
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
    Some(LoadedManifest {
        result: read_manifest(&path),
        path,
    })
}

/// Read and parse one manifest by its exact path, without ancestor discovery.
pub(crate) fn read_manifest(path: &Path) -> Result<ProjectManifest, ManifestDiagnostic> {
    fs::read_to_string(path)
        .map_err(|error| ManifestDiagnostic {
            message: format!("could not read Aster.toml: {error}"),
            help: "make sure `Aster.toml` is readable UTF-8 text".to_owned(),
        })
        .and_then(|source| parse_manifest(&source))
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
            "use `schema = 2` followed by `[package]` and, for an application, `[application]`",
        )
    })?;
    let table = document.as_table().ok_or_else(|| {
        manifest_error(
            "Aster.toml must contain a TOML table",
            "use `schema = 2` followed by a `[package]` table",
        )
    })?;

    // A manifest without `schema` predates the field and keeps schema 1's
    // documented meaning.
    let schema = match table.get("schema") {
        None => 1,
        Some(toml::Value::Integer(value)) if SUPPORTED_SCHEMAS.contains(value) => *value,
        Some(toml::Value::Integer(value)) => {
            return Err(manifest_error(
                format!("unsupported Aster.toml schema `{value}`"),
                format!(
                    "this ASTER build supports schema {}; update the toolchain or migrate the manifest",
                    supported_schema_list()
                ),
            ));
        }
        Some(_) => {
            return Err(manifest_error(
                "Aster.toml `schema` must be an integer",
                format!("write `schema = {CURRENT_MANIFEST_SCHEMA}`"),
            ));
        }
    };

    if schema == 1 {
        parse_schema_1(table)
    } else {
        parse_schema_2(table)
    }
}

/// Schema 1 keeps its original closed meaning: only `schema` and a required
/// `[application]` table carrying only `entry`.
fn parse_schema_1(table: &toml::Table) -> Result<ProjectManifest, ManifestDiagnostic> {
    if let Some(key) = table
        .keys()
        .find(|key| !matches!(key.as_str(), "schema" | "application"))
    {
        return Err(manifest_error(
            format!("unknown top-level Aster.toml field `{key}`"),
            format!(
                "schema 1 supports only `schema` and `[application]`; write `schema = {CURRENT_MANIFEST_SCHEMA}` to use `[package]` and `[dependencies]`"
            ),
        ));
    }

    let application = table.get("application").ok_or_else(|| {
        manifest_error(
            "Aster.toml does not define an `[application]` table",
            "add `[application]` and `entry = \"app.Program.Main\"`",
        )
    })?;
    let application = parse_application(application)?;

    Ok(ProjectManifest {
        schema: 1,
        package: None,
        application: Some(application),
        dependencies: Vec::new(),
    })
}

/// Schema 2 adds package identity and local path dependencies, and makes
/// `[application]` optional so a package can be a pure library.
fn parse_schema_2(table: &toml::Table) -> Result<ProjectManifest, ManifestDiagnostic> {
    if let Some(key) = table.keys().find(|key| {
        !matches!(
            key.as_str(),
            "schema" | "package" | "application" | "dependencies"
        )
    }) {
        return Err(manifest_error(
            format!("unknown top-level Aster.toml field `{key}`"),
            "schema 2 supports only `schema`, `[package]`, `[application]`, and `[dependencies]`",
        ));
    }

    let package = table.get("package").ok_or_else(|| {
        manifest_error(
            "Aster.toml does not define a `[package]` table",
            "add `[package]` and `name = \"my_package\"`",
        )
    })?;
    let package = parse_package(package)?;

    let application = table
        .get("application")
        .map(parse_application)
        .transpose()?;
    let dependencies = match table.get("dependencies") {
        None => Vec::new(),
        Some(value) => parse_dependencies(value)?,
    };

    Ok(ProjectManifest {
        schema: 2,
        package: Some(package),
        application,
        dependencies,
    })
}

fn parse_package(value: &toml::Value) -> Result<PackageManifest, ManifestDiagnostic> {
    let table = value.as_table().ok_or_else(|| {
        manifest_error(
            "Aster.toml `package` must be a table",
            "use `[package]` followed by `name = \"my_package\"`",
        )
    })?;
    if let Some(key) = table.keys().find(|key| key.as_str() != "name") {
        return Err(manifest_error(
            format!("unknown Aster.toml package field `{key}`"),
            "schema 2 supports only `package.name`",
        ));
    }
    let name = table.get("name").ok_or_else(|| {
        manifest_error(
            "Aster.toml does not define `package.name`",
            "add `name = \"my_package\"` below `[package]`",
        )
    })?;
    let name = name.as_str().ok_or_else(|| {
        manifest_error(
            "Aster.toml `package.name` must be a string",
            "write `name = \"my_package\"`",
        )
    })?;
    if !valid_identifier(name) {
        return Err(manifest_error(
            format!("package name `{name}` is not a valid ASTER identifier"),
            "use letters, digits, and underscores, starting with a letter or underscore",
        ));
    }
    Ok(PackageManifest {
        name: name.to_owned(),
    })
}

fn parse_dependencies(value: &toml::Value) -> Result<Vec<DependencyManifest>, ManifestDiagnostic> {
    let table = value.as_table().ok_or_else(|| {
        manifest_error(
            "Aster.toml `dependencies` must be a table",
            "use `[dependencies]` followed by `name = { path = \"../name\" }`",
        )
    })?;
    let mut dependencies = Vec::new();
    for (name, entry) in table {
        if !valid_identifier(name) {
            return Err(manifest_error(
                format!("dependency name `{name}` is not a valid ASTER identifier"),
                "use letters, digits, and underscores, starting with a letter or underscore",
            ));
        }
        let entry = entry.as_table().ok_or_else(|| {
            manifest_error(
                format!("dependency `{name}` must be a table"),
                format!("write `{name} = {{ path = \"../{name}\" }}`"),
            )
        })?;
        if let Some(key) = entry.keys().find(|key| key.as_str() != "path") {
            return Err(manifest_error(
                format!("unknown field `{key}` in dependency `{name}`"),
                "schema 2 supports only `path` dependencies; Git and registry sources are not implemented",
            ));
        }
        let path = entry.get("path").ok_or_else(|| {
            manifest_error(
                format!("dependency `{name}` does not define `path`"),
                format!("write `{name} = {{ path = \"../{name}\" }}`"),
            )
        })?;
        let path = path.as_str().ok_or_else(|| {
            manifest_error(
                format!("dependency `{name}` path must be a string"),
                format!("write `{name} = {{ path = \"../{name}\" }}`"),
            )
        })?;
        if path.is_empty() {
            return Err(manifest_error(
                format!("dependency `{name}` has an empty path"),
                format!("write `{name} = {{ path = \"../{name}\" }}`"),
            ));
        }
        dependencies.push(DependencyManifest {
            name: name.clone(),
            path: path.to_owned(),
        });
    }
    // TOML table order is not a public contract, so the graph is ordered here.
    dependencies.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(dependencies)
}

fn parse_application(value: &toml::Value) -> Result<ApplicationManifest, ManifestDiagnostic> {
    let application = value.as_table().ok_or_else(|| {
        manifest_error(
            "Aster.toml `application` must be a table",
            "use `[application]` followed by `entry = \"app.Program.Main\"`",
        )
    })?;

    if let Some(key) = application.keys().find(|key| key.as_str() != "entry") {
        return Err(manifest_error(
            format!("unknown Aster.toml application field `{key}`"),
            "an `[application]` table supports only `entry`",
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

    Ok(ApplicationManifest {
        entry: ManifestEntry {
            namespace: parts[..parts.len() - 2].join("."),
            class: parts[parts.len() - 2].to_owned(),
            method,
        },
    })
}

fn supported_schema_list() -> String {
    SUPPORTED_SCHEMAS
        .iter()
        .map(|schema| format!("`{schema}`"))
        .collect::<Vec<_>>()
        .join(" and ")
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

#[cfg(test)]
mod tests {
    use super::{CURRENT_MANIFEST_SCHEMA, parse_manifest};

    #[test]
    fn schema_one_keeps_its_closed_meaning() {
        let manifest =
            parse_manifest("schema = 1\n\n[application]\nentry = \"app.Program.Main\"\n")
                .expect("schema 1 manifest");
        assert_eq!(manifest.schema, 1);
        assert!(manifest.package.is_none());
        assert!(manifest.dependencies.is_empty());
        assert_eq!(
            manifest.application.expect("application").entry.namespace,
            "app"
        );
    }

    #[test]
    fn a_manifest_without_schema_is_schema_one() {
        let manifest = parse_manifest("[application]\nentry = \"app.Program.Main\"\n")
            .expect("implicit schema 1");
        assert_eq!(manifest.schema, 1);
    }

    #[test]
    fn schema_one_still_rejects_package_and_dependencies() {
        for source in [
            "schema = 1\n\n[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n",
            "schema = 1\n\n[application]\nentry = \"app.Program.Main\"\n\n[dependencies]\nmath = { path = \"../math\" }\n",
        ] {
            let error = parse_manifest(source).expect_err("schema 1 is closed");
            assert!(
                error.message.contains("unknown top-level Aster.toml field"),
                "{}",
                error.message
            );
        }
    }

    #[test]
    fn schema_two_requires_package_identity() {
        let error = parse_manifest("schema = 2\n\n[application]\nentry = \"app.Program.Main\"\n")
            .expect_err("schema 2 requires a package name");
        assert!(
            error.message.contains("`[package]` table"),
            "{}",
            error.message
        );
    }

    #[test]
    fn schema_two_allows_a_library_without_an_application() {
        let manifest =
            parse_manifest("schema = 2\n\n[package]\nname = \"math\"\n").expect("library package");
        assert_eq!(manifest.package.expect("package").name, "math");
        assert!(manifest.application.is_none());
    }

    #[test]
    fn dependencies_are_sorted_by_declared_name() {
        let manifest = parse_manifest(
            "schema = 2\n\n[package]\nname = \"app\"\n\n[dependencies]\nzeta = { path = \"../zeta\" }\nalpha = { path = \"../alpha\" }\n",
        )
        .expect("dependency table");
        let names = manifest
            .dependencies
            .iter()
            .map(|dependency| dependency.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["alpha", "zeta"]);
    }

    #[test]
    fn unsupported_schema_is_rejected_rather_than_guessed() {
        let error = parse_manifest("schema = 999\n").expect_err("unsupported schema");
        assert!(
            error
                .message
                .contains("unsupported Aster.toml schema `999`")
        );
        assert!(error.help.contains("`1`"));
        assert!(error.help.contains("`2`"));
    }

    #[test]
    fn git_and_registry_dependency_sources_are_rejected() {
        let error = parse_manifest(
            "schema = 2\n\n[package]\nname = \"app\"\n\n[dependencies]\nmath = { git = \"https://example.invalid/math\" }\n",
        )
        .expect_err("only path dependencies exist");
        assert!(
            error.message.contains("unknown field `git`"),
            "{}",
            error.message
        );
    }

    #[test]
    fn current_schema_is_the_newest_supported_schema() {
        assert_eq!(CURRENT_MANIFEST_SCHEMA, 2);
    }
}
