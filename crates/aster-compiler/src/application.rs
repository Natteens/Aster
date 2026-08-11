//! Application entry-point selection after typed manifest loading.
//!
//! This layer runs after project linking and semantic analysis. It selects a
//! resolved HIR function symbol; neither MIR nor the backend knows about TOML,
//! source paths, or the `Main` convention.

use std::path::{Path, PathBuf};

use aster_hir::{self as hir, SymbolId, Type, Visibility};

use crate::{
    ProjectCompilation,
    manifest::{ManifestEntry, load_manifest},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationEntry {
    pub symbol: SymbolId,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationDiagnostic {
    pub path: PathBuf,
    pub message: String,
    pub help: String,
}

impl ApplicationDiagnostic {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "error[{}]: {}\nhelp: {}",
            self.path.display(),
            self.message,
            self.help
        )
    }
}

/// Select a manifest entry when present, otherwise select a conventional
/// public static `Main` method from a root-namespace class.
///
/// # Errors
///
/// Returns path-aware diagnostics when the manifest or candidate method does
/// not satisfy the application entry contract.
pub fn select_application_entry(
    project: &ProjectCompilation,
    root_file: &Path,
) -> Result<ApplicationEntry, Vec<ApplicationDiagnostic>> {
    match load_manifest_entry(root_file)? {
        ManifestApplication::Configured(path, entry) => {
            select_manifest_entry(project, &path, &entry)
        }
        ManifestApplication::Library(path) => Err(vec![diagnostic(
            &path,
            "this package does not define an `[application]` entry",
            "add `[application]` with `entry = \"app.Program.Main\"`, or use `aster check` for a library package",
        )]),
        ManifestApplication::Manifestless => select_conventional_entry(project, root_file),
    }
}

enum ManifestApplication {
    Configured(PathBuf, ManifestEntry),
    Library(PathBuf),
    Manifestless,
}

fn load_manifest_entry(
    root_file: &Path,
) -> Result<ManifestApplication, Vec<ApplicationDiagnostic>> {
    let Some(manifest) = load_manifest(root_file) else {
        return Ok(ManifestApplication::Manifestless);
    };
    let path = manifest.path;
    let manifest = manifest
        .result
        .map_err(|error| vec![diagnostic(&path, error.message, error.help)])?;
    Ok(match manifest.application {
        Some(application) => ManifestApplication::Configured(path, application.entry),
        None => ManifestApplication::Library(path),
    })
}

fn select_manifest_entry(
    project: &ProjectCompilation,
    path: &Path,
    entry: &ManifestEntry,
) -> Result<ApplicationEntry, Vec<ApplicationDiagnostic>> {
    if crate::standard_library::is_official_name(&entry.namespace) {
        return Err(vec![diagnostic(
            path,
            format!(
                "application entry cannot point into standard library namespace `{}`",
                entry.namespace
            ),
            "declare the application entry in a project namespace",
        )]);
    }
    let candidates = entry_identity_candidates(project, entry);
    let Some(class) = classes(&project.compilation.hir)
        .find(|class| candidates.iter().any(|candidate| candidate == &class.name))
    else {
        return Err(vec![diagnostic(
            path,
            format!(
                "entry type `{}.{}` was not found in the root namespace or its using graph",
                entry.namespace, entry.class
            ),
            "declare the namespace and add the necessary using, then check the class spelling",
        )]);
    };
    if class.visibility != Visibility::Public {
        return Err(vec![diagnostic(
            path,
            format!(
                "entry class `{}.{}` is not public",
                entry.namespace, entry.class
            ),
            "declare the entry class as `public`",
        )]);
    }
    select_named_methods(
        class,
        &entry.method,
        path,
        &format!("{}.{}.{}", entry.namespace, entry.class, entry.method),
    )
}

/// Every compiler-internal identity the entry class could resolve to.
/// Manifest-backed packages have one package-qualified identity regardless of
/// which package file was the literal compilation root. The empty-package
/// branch is reserved for the direct-file workflow without `Aster.toml`.
fn entry_identity_candidates(project: &ProjectCompilation, entry: &ManifestEntry) -> Vec<String> {
    let mut candidates = Vec::new();
    let namespace_scoped = format!("{}::{}", entry.namespace, entry.class);
    match project.root_package_name() {
        "" => {
            if project.root_namespace == entry.namespace {
                candidates.push(entry.class.clone());
            }
            candidates.push(namespace_scoped);
        }
        package => candidates.push(format!("{package}::{namespace_scoped}")),
    }
    candidates
}

fn select_conventional_entry(
    project: &ProjectCompilation,
    root_file: &Path,
) -> Result<ApplicationEntry, Vec<ApplicationDiagnostic>> {
    let root_classes = classes(&project.compilation.hir)
        .filter(|class| project.is_root_type(&class.name))
        .collect::<Vec<_>>();
    let candidates = root_classes
        .iter()
        .flat_map(|class| {
            class
                .methods
                .iter()
                .filter(|method| !method.constructor && method.name == "Main")
                .map(move |method| (*class, method))
        })
        .collect::<Vec<_>>();
    let valid = candidates
        .iter()
        .filter(|(class, method)| entry_problems(class, method).is_empty())
        .collect::<Vec<_>>();
    if valid.len() == 1 {
        let (class, method) = valid[0];
        return Ok(ApplicationEntry {
            symbol: method.symbol,
            display_name: format!("{}.Main", class.name),
        });
    }
    if valid.len() > 1 {
        let names = valid
            .iter()
            .map(|(class, _)| format!("{}.Main", class.name))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(vec![diagnostic(
            root_file,
            format!("more than one conventional application entry was found: {names}"),
            "keep one eligible `public static Main`, or choose one with `Aster.toml`",
        )]);
    }
    if candidates.is_empty() {
        return Err(vec![diagnostic(
            root_file,
            "no application entry was found in the root namespace",
            "add `public static void Main()` or `public static int Main()` to a public class, or pass `--function NAME`",
        )]);
    }
    let diagnostics = candidates
        .into_iter()
        .flat_map(|(class, method)| {
            entry_problems(class, method).into_iter().map(move |problem| {
                diagnostic(
                    root_file,
                    format!("`{}.Main` cannot be used as the application entry: {problem}", class.name),
                    "use a public class and declare `public static void Main()` or `public static int Main()` with no parameters",
                )
            })
        })
        .collect();
    Err(diagnostics)
}

fn select_named_methods(
    class: &hir::TypeDeclaration,
    method_name: &str,
    path: &Path,
    display_name: &str,
) -> Result<ApplicationEntry, Vec<ApplicationDiagnostic>> {
    let methods = class
        .methods
        .iter()
        .filter(|method| !method.constructor && method.name == method_name)
        .collect::<Vec<_>>();
    if methods.is_empty() {
        return Err(vec![diagnostic(
            path,
            format!("entry method `{display_name}` was not found"),
            "declare the configured method as `public static void Main()` or `public static int Main()`",
        )]);
    }
    let valid = methods
        .iter()
        .filter(|method| entry_problems(class, method).is_empty())
        .collect::<Vec<_>>();
    if valid.len() == 1 {
        return Ok(ApplicationEntry {
            symbol: valid[0].symbol,
            display_name: display_name.to_owned(),
        });
    }
    if valid.len() > 1 {
        return Err(vec![diagnostic(
            path,
            format!("entry `{display_name}` resolves to more than one eligible method"),
            "leave only one public static parameterless `Main` overload",
        )]);
    }
    Err(methods
        .into_iter()
        .flat_map(|method| {
            entry_problems(class, method).into_iter().map(|problem| {
                diagnostic(
                    path,
                    format!("entry `{display_name}` is invalid: {problem}"),
                    "use a public class and declare `public static void Main()` or `public static int Main()` with no parameters",
                )
            })
        })
        .collect())
}

fn entry_problems(class: &hir::TypeDeclaration, method: &hir::Function) -> Vec<&'static str> {
    let mut problems = Vec::new();
    if class.visibility != Visibility::Public {
        problems.push("its declaring class is not public");
    }
    if method.visibility != Visibility::Public {
        problems.push("the method is not public");
    }
    if !method.is_static {
        problems.push("the method is not static");
    }
    if !method.parameters.is_empty() {
        problems.push("the method has parameters");
    }
    if !matches!(method.return_type, Type::Void | Type::Int) {
        problems.push("the return type is not `void` or `int`");
    }
    problems
}

fn classes(module: &hir::Module) -> impl Iterator<Item = &hir::TypeDeclaration> {
    module.items.iter().filter_map(|item| {
        let hir::Item::Class(class) = item else {
            return None;
        };
        Some(class)
    })
}

fn diagnostic(
    path: &Path,
    message: impl Into<String>,
    help: impl Into<String>,
) -> ApplicationDiagnostic {
    ApplicationDiagnostic {
        path: path.to_path_buf(),
        message: message.into(),
        help: help.into(),
    }
}
