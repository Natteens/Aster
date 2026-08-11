//! Typed `Aster.toml` loading and project-root discovery.
//!
//! This module is the compiler-side authority for the current manifest
//! format. Downstream semantic, HIR, MIR, backend, and runtime layers never
//! depend on TOML or source paths.

use std::{
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectManifest {
    /// Declared package identity for every manifest-backed package.
    pub package: PackageManifest,
    /// Present when the package can be executed. A package without one is a
    /// reusable source package.
    pub application: Option<ApplicationManifest>,
    /// Direct dependencies, sorted by declared name.
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
    pub source: DependencySource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DependencySource {
    /// Raw path text, interpreted relative to the declaring manifest.
    Path { path: String },
    /// Public HTTPS Git repository and the exact user-declared revision.
    Git { git: String, rev: String },
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
        if let Some(key) = entry
            .keys()
            .find(|key| !matches!(key.as_str(), "path" | "git" | "rev"))
        {
            return Err(manifest_error(
                format!("unknown field `{key}` in dependency `{name}`"),
                "dependencies support only `path`, or `git` together with `rev`",
            ));
        }
        let source = match (entry.get("path"), entry.get("git"), entry.get("rev")) {
            (Some(_), Some(_), _) => {
                return Err(manifest_error(
                    format!("dependency `{name}` cannot define both `path` and `git`"),
                    "choose one dependency source",
                ));
            }
            (Some(path), None, None) => {
                let path = dependency_string(path, name, "path")?;
                if path.is_empty() {
                    return Err(manifest_error(
                        format!("dependency `{name}` has an empty path"),
                        format!("write `{name} = {{ path = \"../{name}\" }}`"),
                    ));
                }
                DependencySource::Path { path }
            }
            (Some(_), None, Some(_)) => {
                return Err(manifest_error(
                    format!("path dependency `{name}` cannot define `rev`"),
                    "remove `rev` from the path dependency",
                ));
            }
            (None, Some(git), Some(rev)) => {
                let git = dependency_string(git, name, "git")?;
                let rev = dependency_string(rev, name, "rev")?;
                validate_git_url(&git).map_err(|message| {
                    manifest_error(
                        format!("dependency `{name}` {message}"),
                        "use a public HTTPS repository URL",
                    )
                })?;
                validate_git_rev(&rev).map_err(|message| {
                    manifest_error(
                        format!("dependency `{name}` {message}"),
                        "use a branch, tag, or full commit SHA",
                    )
                })?;
                DependencySource::Git { git, rev }
            }
            (None, Some(_), None) => {
                return Err(manifest_error(
                    format!("Git dependency `{name}` does not define `rev`"),
                    format!("write `{name} = {{ git = \"https://...\", rev = \"main\" }}"),
                ));
            }
            (None, None, Some(_)) => {
                return Err(manifest_error(
                    format!("dependency `{name}` defines `rev` without `git`"),
                    "add `git` or remove `rev`",
                ));
            }
            (None, None, None) => {
                return Err(manifest_error(
                    format!("dependency `{name}` does not define a source"),
                    "define either `path`, or `git` together with `rev`",
                ));
            }
        };
        dependencies.push(DependencyManifest {
            name: name.clone(),
            source,
        });
    }
    // TOML table order is not a public contract, so the graph is ordered here.
    dependencies.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(dependencies)
}

fn dependency_string(
    value: &toml::Value,
    name: &str,
    field: &str,
) -> Result<String, ManifestDiagnostic> {
    value.as_str().map(str::to_owned).ok_or_else(|| {
        manifest_error(
            format!("dependency `{name}` {field} must be a string"),
            format!("write `{field} = \"...\"`"),
        )
    })
}

pub(crate) fn validate_git_url(url: &str) -> Result<(), &'static str> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err("Git URL must use `https://`");
    };
    if rest.is_empty() || rest.chars().any(char::is_control) || rest.contains(['\\', '?', '#', '@'])
    {
        return Err("has an invalid Git URL");
    }
    let Some((host, path)) = rest.split_once('/') else {
        return Err("Git URL must include a repository path");
    };
    if host.is_empty()
        || path.is_empty()
        || host.contains(':')
        || host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok()
        || !host.contains('.')
        || !host.is_ascii()
        || host.split('.').any(str::is_empty)
        || path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err("has an invalid public HTTPS Git URL");
    }
    Ok(())
}

pub(crate) fn validate_git_rev(rev: &str) -> Result<(), &'static str> {
    if rev.is_empty()
        || rev.chars().any(char::is_control)
        || rev.starts_with('-')
        || rev.contains([' ', '~', '^', ':', '?', '*', '[', '\\'])
        || rev.contains("..")
        || rev.contains("//")
        || rev.starts_with('/')
        || rev.ends_with(['.', '/'])
        || rev.contains("@{")
        || rev.split('/').any(|component| {
            component.starts_with('.')
                || Path::new(component)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lock"))
        })
    {
        return Err("has an invalid Git revision");
    }
    if rev.chars().all(|character| character.is_ascii_hexdigit()) && !matches!(rev.len(), 40 | 64) {
        return Err("uses a short commit SHA; use the full SHA");
    }
    Ok(())
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

pub(crate) fn valid_identifier(value: &str) -> bool {
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
    fn git_dependencies_require_a_public_https_url_and_revision() {
        let manifest = parse_manifest(
            "[package]\nname = \"app\"\n\n[dependencies]\nmath = { git = \"https://example.invalid/math.git\", rev = \"main\" }\n",
        )
        .expect("Git dependency");
        assert!(matches!(
            &manifest.dependencies[0].source,
            super::DependencySource::Git { git, rev }
                if git == "https://example.invalid/math.git" && rev == "main"
        ));

        for entry in [
            "math = { git = \"https://example.invalid/math.git\" }",
            "math = { git = \"ssh://example.invalid/math.git\", rev = \"main\" }",
            "math = { git = \"https://user@example.invalid/math.git\", rev = \"main\" }",
            "math = { git = \"https://example.invalid/math.git\", rev = \"abc123\" }",
            "math = { path = \"../math\", git = \"https://example.invalid/math.git\", rev = \"main\" }",
            "math = { git = \"https://example.invalid/math.git\", rev = \"main\", branch = \"main\" }",
        ] {
            let error = parse_manifest(&format!(
                "[package]\nname = \"app\"\n\n[dependencies]\n{entry}\n"
            ))
            .expect_err("invalid Git dependency");
            assert!(!error.message.is_empty());
        }
    }
}
