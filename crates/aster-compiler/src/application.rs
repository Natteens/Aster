//! Application manifest loading and semantic entry-point selection.
//!
//! This layer runs after project linking and semantic analysis. It selects a
//! resolved HIR function symbol; neither MIR nor the backend knows about TOML,
//! source paths, or the `Main` convention.

use std::{
    fs,
    path::{Path, PathBuf},
};

use aster_hir::{self as hir, SymbolId, Type, Visibility};

use crate::ProjectCompilation;

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

#[derive(Clone, Debug)]
struct ManifestEntry {
    path: PathBuf,
    namespace: String,
    class: String,
    method: String,
}

/// Find the nearest `Aster.toml`, beginning at the root source directory.
#[must_use]
pub fn find_manifest_path(root_file: &Path) -> Option<PathBuf> {
    let absolute = fs::canonicalize(root_file).ok()?;
    for directory in absolute.parent()?.ancestors() {
        let candidate = directory.join("Aster.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
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
        Some(entry) => select_manifest_entry(project, &entry),
        None => select_conventional_entry(project, root_file),
    }
}

fn load_manifest_entry(
    root_file: &Path,
) -> Result<Option<ManifestEntry>, Vec<ApplicationDiagnostic>> {
    let Some(path) = find_manifest_path(root_file) else {
        return Ok(None);
    };
    let source = fs::read_to_string(&path).map_err(|error| {
        vec![diagnostic(
            &path,
            format!("could not read application manifest: {error}"),
            "make sure `Aster.toml` is readable UTF-8 text",
        )]
    })?;
    let document = source.parse::<toml::Value>().map_err(|error| {
        vec![diagnostic(
            &path,
            format!("invalid Aster.toml: {error}"),
            "use `[application]` followed by `entry = \"namespace.Class.Main\"`",
        )]
    })?;
    let entry = document
        .get("application")
        .and_then(toml::Value::as_table)
        .and_then(|application| application.get("entry"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            vec![diagnostic(
                &path,
                "Aster.toml does not define a string `application.entry`",
                "add `[application]` and `entry = \"app.Program.Main\"`",
            )]
        })?;
    let parts = entry.split('.').collect::<Vec<_>>();
    if parts.len() < 3 || parts.iter().any(|part| !valid_identifier(part)) {
        return Err(vec![diagnostic(
            &path,
            format!("application entry `{entry}` has an invalid format"),
            "use a dotted `namespace.Class.Main` name such as `app.Program.Main`",
        )]);
    }
    let method = parts[parts.len() - 1].to_owned();
    if method != "Main" {
        return Err(vec![diagnostic(
            &path,
            format!("application entry must end in `.Main`, but found `.{method}`"),
            "point `application.entry` at a public static `Main` method",
        )]);
    }
    Ok(Some(ManifestEntry {
        path,
        namespace: parts[..parts.len() - 2].join("."),
        class: parts[parts.len() - 2].to_owned(),
        method,
    }))
}

fn select_manifest_entry(
    project: &ProjectCompilation,
    entry: &ManifestEntry,
) -> Result<ApplicationEntry, Vec<ApplicationDiagnostic>> {
    if crate::standard_library::is_official_name(&entry.namespace) {
        return Err(vec![diagnostic(
            &entry.path,
            format!(
                "application entry cannot point into standard library namespace `{}`",
                entry.namespace
            ),
            "declare the application entry in a project namespace",
        )]);
    }
    let linked_name = if project.root_namespace == entry.namespace {
        entry.class.clone()
    } else {
        format!("{}::{}", entry.namespace, entry.class)
    };
    let Some(class) = classes(&project.compilation.hir).find(|class| class.name == linked_name)
    else {
        return Err(vec![diagnostic(
            &entry.path,
            format!(
                "entry type `{}.{}` was not found in the root namespace or its using graph",
                entry.namespace, entry.class
            ),
            "declare the namespace and add the necessary using, then check the class spelling",
        )]);
    };
    if class.visibility != Visibility::Public {
        return Err(vec![diagnostic(
            &entry.path,
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
        &entry.path,
        &format!("{}.{}.{}", entry.namespace, entry.class, entry.method),
    )
}

fn select_conventional_entry(
    project: &ProjectCompilation,
    root_file: &Path,
) -> Result<ApplicationEntry, Vec<ApplicationDiagnostic>> {
    let root_classes = classes(&project.compilation.hir)
        .filter(|class| !class.name.contains("::"))
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

fn valid_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some('_' | 'a'..='z' | 'A'..='Z'))
        && characters.all(|character| matches!(character, '_' | 'a'..='z' | 'A'..='Z' | '0'..='9'))
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
