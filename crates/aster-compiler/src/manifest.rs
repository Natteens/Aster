//! Typed `Aster.toml` loading and project-root discovery.
//!
//! This module is the compiler-side authority for the current manifest
//! format. Downstream semantic, HIR, MIR, backend, and runtime layers never
//! depend on TOML or source paths.

use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectManifest {
    /// Declared package identity for every manifest-backed package.
    pub package: PackageManifest,
    /// Present when the package can be executed. A package without one is a
    /// reusable source package.
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
            "use `[package]` and, for an application, `[application]`",
        )
    })?;
    let table = document.as_table().ok_or_else(|| {
        manifest_error(
            "Aster.toml must contain a TOML table",
            "use a `[package]` table",
        )
    })?;

    if table.contains_key("schema") {
        return Err(manifest_error(
            "Aster.toml no longer uses a `schema` field; remove it",
            "start the manifest with a `[package]` table",
        ));
    }
    if let Some(key) = table
        .keys()
        .find(|key| !matches!(key.as_str(), "package" | "application" | "dependencies"))
    {
        return Err(manifest_error(
            format!("unknown top-level Aster.toml field `{key}`"),
            "Aster.toml supports only `[package]`, `[application]`, and `[dependencies]`",
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
        package,
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
            "a `[package]` table supports only `name`",
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
                "only `path` dependencies are implemented; Git and registry sources are not available",
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
    use super::parse_manifest;

    #[test]
    fn an_application_manifest_uses_the_current_package_format() {
        let manifest = parse_manifest(
            "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n",
        )
        .expect("application manifest");
        assert_eq!(manifest.package.name, "app");
        assert!(manifest.dependencies.is_empty());
        assert_eq!(
            manifest.application.expect("application").entry.namespace,
            "app"
        );
    }

    #[test]
    fn every_manifest_requires_package_identity() {
        let error = parse_manifest("[application]\nentry = \"app.Program.Main\"\n")
            .expect_err("a manifest requires a package name");
        assert!(
            error.message.contains("`[package]` table"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_library_needs_no_application_table() {
        let manifest = parse_manifest("[package]\nname = \"math\"\n").expect("library package");
        assert_eq!(manifest.package.name, "math");
        assert!(manifest.application.is_none());
    }

    #[test]
    fn dependencies_are_sorted_by_declared_name() {
        let manifest = parse_manifest(
            "[package]\nname = \"app\"\n\n[dependencies]\nzeta = { path = \"../zeta\" }\nalpha = { path = \"../alpha\" }\n",
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
    fn schema_fields_are_rejected_without_activating_old_semantics() {
        for schema in ["1", "2", "999", "\"future\""] {
            let source = format!(
                "schema = {schema}\n\n[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n"
            );
            let error = parse_manifest(&source).expect_err("schema fields are not supported");
            assert_eq!(
                error.message,
                "Aster.toml no longer uses a `schema` field; remove it"
            );
        }
    }

    #[test]
    fn git_and_registry_dependency_sources_are_rejected() {
        let error = parse_manifest(
            "[package]\nname = \"app\"\n\n[dependencies]\nmath = { git = \"https://example.invalid/math\" }\n",
        )
        .expect_err("only path dependencies exist");
        assert!(
            error.message.contains("unknown field `git`"),
            "{}",
            error.message
        );
    }
}
