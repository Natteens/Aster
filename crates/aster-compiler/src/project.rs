//! Local multi-file project loading and name linking.
//!
//! Files are resolved here, before semantic analysis. The linked AST uses
//! qualified internal names for linked declarations, so later stages remain
//! independent of paths and `using` syntax.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use aster_diagnostics::{Diagnostic, Span};
use aster_syntax::{
    Expression, ExpressionKind, FunctionDeclaration, Item, Member, Module, Property, Statement,
    SwitchCase, Token, TypeDeclaration, TypeRef, VariableDeclaration, Visibility, lex, parse,
    visit::{
        AstVisitorMut, walk_expression_mut, walk_function_declaration_mut, walk_statement_mut,
        walk_switch_case_mut, walk_type_declaration_mut, walk_variable_declaration_mut,
    },
};

use crate::standard_library::{StandardLibrary, is_official_name};
use crate::{Compilation, compile_module};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectSourceOrigin {
    Project,
    StandardLibrary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectSource {
    pub path: PathBuf,
    pub source: String,
    pub offset: usize,
    pub origin: ProjectSourceOrigin,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectDiagnostic {
    pub path: PathBuf,
    pub source: String,
    pub diagnostic: Diagnostic,
}

impl ProjectDiagnostic {
    #[must_use]
    pub fn render(&self) -> String {
        self.diagnostic
            .render(&self.path.to_string_lossy(), &self.source)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectCompilation {
    pub compilation: Compilation,
    pub sources: Vec<ProjectSource>,
    pub root_namespace: String,
    root_public_functions: HashSet<String>,
    /// Compiler-internal identities of types declared in the literal root
    /// file. Application entry selection uses this instead of inferring root
    /// ownership from the textual shape of a linked name.
    root_type_names: HashSet<String>,
    /// Every manifest that participated in the resolved package graph, sorted.
    /// Editing one of these can change compilation, so watch mode observes them.
    manifest_paths: Vec<PathBuf>,
    requires_application_entry: bool,
    /// The root package's declared `[package] name`, or empty for a
    /// manifest-less root. See [`linked_name`] for how this participates in
    /// compiler-internal nominal identity.
    root_package_name: String,
    tests: Vec<TestDescriptor>,
}

/// One compiler-discovered root-package test callable. The descriptor is
/// deliberately outside HIR/MIR: execution still invokes an ordinary concrete
/// function symbol selected before the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestDescriptor {
    pub symbol: aster_hir::SymbolId,
    pub display_name: String,
}

impl ProjectCompilation {
    /// Every local input whose change can affect this compilation: project
    /// sources from the resolved package graph, plus every participating
    /// manifest and the root lockfile. Standard-library sources are excluded.
    #[must_use]
    pub fn dependency_paths(&self) -> Vec<PathBuf> {
        let mut paths = self
            .sources
            .iter()
            .filter(|source| source.origin == ProjectSourceOrigin::Project)
            .map(|source| source.path.clone())
            .collect::<Vec<_>>();
        paths.extend(self.manifest_paths.iter().cloned());
        paths.sort();
        paths.dedup();
        paths
    }

    #[must_use]
    pub fn is_root_public_function(&self, name: &str) -> bool {
        self.root_public_functions.contains(name)
    }

    /// Resolve the source-level name accepted by `--function` to the symbol
    /// produced after package-aware linking.
    #[must_use]
    pub fn root_public_function_symbol(&self, name: &str) -> Option<aster_hir::SymbolId> {
        if !self.root_public_functions.contains(name) {
            return None;
        }
        let identity = self.root_item_identity(name);
        self.compilation
            .hir
            .items
            .iter()
            .find_map(|item| match item {
                aster_hir::Item::Function(function) if function.name == identity => {
                    Some(function.symbol)
                }
                _ => None,
            })
    }

    pub(crate) fn is_root_type(&self, name: &str) -> bool {
        self.root_type_names.contains(name)
    }

    fn root_item_identity(&self, name: &str) -> String {
        if self.root_package_name.is_empty() {
            name.to_owned()
        } else {
            format!(
                "{}::{}",
                self.root_package_name,
                namespace_scoped_name(&self.root_namespace, name)
            )
        }
    }

    /// Whether an application entry must be selected for this project: a root
    /// manifest exists and either declares `[application]` or failed to parse.
    /// A library package declares neither and is not forced to provide `Main`.
    #[must_use]
    pub fn requires_application_entry(&self) -> bool {
        self.requires_application_entry
    }

    /// The root package's declared name, or `""` when there is no manifest.
    /// Crate-internal: this is a compiler identity detail, not ASTER source
    /// syntax.
    #[must_use]
    pub(crate) fn root_package_name(&self) -> &str {
        &self.root_package_name
    }

    /// Root-package tests in deterministic display-name order.
    #[must_use]
    pub fn tests(&self) -> &[TestDescriptor] {
        &self.tests
    }
}

/// Index into [`Loader::packages`]. The root package is always index 0.
type PackageId = usize;

const ROOT_PACKAGE: PackageId = 0;

/// One resolved package in the dependency graph.
#[derive(Clone, Debug)]
struct Package {
    /// Declared `[package] name`. Empty only for a manifest-less root or the
    /// standard library, neither of which can declare dependencies.
    name: String,
    /// Canonical directory containing this package's `Aster.toml`, or the root
    /// source directory when no manifest exists.
    root: PathBuf,
    /// Direct dependencies, ordered by declared name.
    dependencies: Vec<PackageId>,
    standard_library: bool,
    /// Root of an immutable materialized Git source. Relative path
    /// dependencies declared by this package may not escape it.
    immutable_source_root: Option<PathBuf>,
}

#[derive(Clone)]
struct Unit {
    path: PathBuf,
    name: String,
    package: PackageId,
    root: bool,
    standard_library: bool,
    tokens: Vec<Token>,
    module: Module,
}

/// Load, link, validate, and lower a root file together with its namespace dependencies.
///
/// # Errors
///
/// Returns sourced diagnostics for file loading, graph resolution, syntax, or
/// semantic failures in any participating compilation unit.
pub fn compile_project(path: &Path) -> Result<ProjectCompilation, Vec<ProjectDiagnostic>> {
    compile_project_with_standard_library(path, StandardLibrary::embedded())
}

/// Compile a root package together with its conventional `tests/` sources.
/// Production commands deliberately use [`compile_project`] instead, so test
/// files never become implicit application input.
///
/// # Errors
///
/// Returns sourced diagnostics for file loading, package resolution, syntax,
/// semantic analysis, or test discovery failures.
pub fn compile_project_for_tests(
    path: &Path,
) -> Result<ProjectCompilation, Vec<ProjectDiagnostic>> {
    compile_project_with_standard_library_for_tests(path, StandardLibrary::embedded())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchSummary {
    pub package_count: usize,
    pub lockfile_path: Option<PathBuf>,
    pub lockfile_changed: bool,
}

/// Resolve and materialize the Git dependencies reachable from one root
/// manifest. This is the only compiler operation allowed to contact remotes.
///
/// # Errors
///
/// Returns sourced diagnostics for an invalid graph, lockfile, cache, or Git
/// source. Existing valid cache entries and lockfiles remain usable on failure.
pub fn fetch_dependencies(
    manifest_path: &Path,
    update: Option<&str>,
) -> Result<FetchSummary, Vec<ProjectDiagnostic>> {
    let manifest_path = fs::canonicalize(manifest_path).map_err(|error| {
        vec![plain_error(
            manifest_path,
            format!("could not read Aster.toml: {error}"),
        )]
    })?;
    if manifest_path.file_name().and_then(|name| name.to_str()) != Some("Aster.toml") {
        return Err(vec![plain_error(
            &manifest_path,
            "fetch requires the root Aster.toml",
        )]);
    }
    let Some(project_root) = manifest_path.parent() else {
        return Err(vec![plain_error(
            &manifest_path,
            "Aster.toml has no parent directory",
        )]);
    };
    if let Err(error) = crate::manifest::read_manifest(&manifest_path) {
        return Err(vec![plain_error(&manifest_path, error.message)]);
    }
    let resolved = resolve_packages(
        Some(&manifest_path),
        project_root,
        ResolutionMode::Fetch { update },
    )
    .map_err(|errors| {
        errors
            .into_iter()
            .map(|error| plain_error(&error.path, error.message))
            .collect::<Vec<_>>()
    })?;
    let ResolutionState::Fetch {
        packages, previous, ..
    } = resolved.state
    else {
        unreachable!("fetch resolution returns fetch state")
    };
    let lockfile_path = project_root.join("Aster.lock");
    if packages.is_empty() {
        let changed = crate::lockfile::remove_atomic(&lockfile_path, previous.as_ref())
            .map_err(|message| vec![plain_error(&lockfile_path, message)])?;
        return Ok(FetchSummary {
            package_count: 0,
            lockfile_path: None,
            lockfile_changed: changed,
        });
    }
    let lockfile = crate::lockfile::Lockfile { packages };
    let changed = crate::lockfile::write_atomic(&lockfile_path, &lockfile, previous.as_ref())
        .map_err(|message| vec![plain_error(&lockfile_path, message)])?;
    Ok(FetchSummary {
        package_count: lockfile.packages.len(),
        lockfile_path: Some(lockfile_path),
        lockfile_changed: changed,
    })
}

pub(crate) fn compile_project_with_standard_library(
    path: &Path,
    standard_library: StandardLibrary,
) -> Result<ProjectCompilation, Vec<ProjectDiagnostic>> {
    compile_project_with_standard_library_inner(path, standard_library, false)
}

pub(crate) fn compile_project_with_standard_library_for_tests(
    path: &Path,
    standard_library: StandardLibrary,
) -> Result<ProjectCompilation, Vec<ProjectDiagnostic>> {
    compile_project_with_standard_library_inner(path, standard_library, true)
}

#[allow(clippy::too_many_lines)]
fn compile_project_with_standard_library_inner(
    path: &Path,
    standard_library: StandardLibrary,
    include_tests: bool,
) -> Result<ProjectCompilation, Vec<ProjectDiagnostic>> {
    let root_path = absolute_path(path).map_err(|message| vec![plain_error(path, message)])?;
    if root_path.extension().and_then(|value| value.to_str()) != Some("aster") {
        return Err(vec![plain_error(
            &root_path,
            "expected a file with the `.aster` extension",
        )]);
    }
    let Some(source_directory) = root_path.parent() else {
        return Err(vec![plain_error(
            &root_path,
            "the root file has no parent directory",
        )]);
    };
    let manifest = crate::find_manifest_path(&root_path);
    let project_root = manifest
        .as_ref()
        .and_then(|manifest| manifest.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| source_directory.to_path_buf());

    // The package graph is resolved from manifests before any source is read,
    // so namespace discovery can never wander outside the declared graph.
    let graph = match resolve_packages(manifest.as_deref(), &project_root, ResolutionMode::Offline)
    {
        Ok(resolved) => resolved.graph,
        Err(errors) => {
            return Err(errors
                .into_iter()
                .map(|error| plain_error(&error.path, error.message))
                .collect());
        }
    };
    let requires_application_entry = graph.requires_application_entry;
    let manifest_paths = graph.manifest_paths.clone();
    let root_package_name = graph.packages[ROOT_PACKAGE].name.clone();

    let mut loader = Loader {
        fallback_path: root_path.clone(),
        packages: graph.packages,
        sources: Vec::new(),
        units: Vec::new(),
        loaded_files: HashSet::new(),
        loaded_namespaces: HashSet::new(),
        loading_namespaces: Vec::new(),
        resolved_usings: HashMap::new(),
        next_offset: 0,
        diagnostics: Vec::new(),
        file_load_diagnostics: Vec::new(),
        standard_library,
    };
    let root_namespace = loader
        .load_file(root_path, ROOT_PACKAGE, true, false, None, None)
        .unwrap_or_default();
    if manifest.is_some()
        && !loader
            .diagnostics
            .iter()
            .any(|value| value.severity == aster_diagnostics::Severity::Error)
        && loader.file_load_diagnostics.is_empty()
    {
        loader.load_namespace(&root_namespace, ROOT_PACKAGE, None);
    }
    let test_files = if include_tests {
        test_source_files(&project_root)?
    } else {
        Vec::new()
    };
    let test_file_paths = test_files.iter().cloned().collect::<HashSet<_>>();
    if include_tests {
        for file in test_files {
            loader.load_file(file, ROOT_PACKAGE, false, false, None, None);
        }
    }
    if !loader.diagnostics.is_empty() || !loader.file_load_diagnostics.is_empty() {
        return Err(loader.finish_diagnostics());
    }
    let root_unit = loader.units.iter().find(|unit| unit.root);
    let root_public_functions = root_unit
        .into_iter()
        .flat_map(|unit| &unit.module.items)
        .filter_map(|item| {
            let Item::Function(function) = item else {
                return None;
            };
            (function.visibility == Visibility::Public).then(|| function.name.clone())
        })
        .collect();
    let root_type_names = linked_root_type_names(root_unit, &loader.packages);
    let mut test_names = std::collections::BTreeSet::new();
    for unit in loader.units.iter().filter(|unit| {
        unit.package == ROOT_PACKAGE
            && !unit.standard_library
            && test_file_paths.contains(&unit.path)
    }) {
        for item in &unit.module.items {
            let Item::Function(function) = item else {
                continue;
            };
            if function.is_test {
                test_names.insert(linked_name(unit, &loader.packages, &function.name));
            }
        }
    }
    let (module, tokens, mut link_diagnostics) =
        link(&loader.units, &loader.packages, &loader.resolved_usings);
    if !link_diagnostics.is_empty() {
        loader.diagnostics.append(&mut link_diagnostics);
        return Err(loader.finish_diagnostics());
    }
    let intrinsic_bindings = loader.standard_library.intrinsic_bindings();
    match compile_module(tokens, module, &intrinsic_bindings, true, true, true) {
        Ok(compilation) => {
            let mut tests = compilation
                .hir
                .items
                .iter()
                .filter_map(|item| {
                    let aster_hir::Item::Function(function) = item else {
                        return None;
                    };
                    test_names.contains(&function.name).then(|| TestDescriptor {
                        symbol: function.symbol,
                        display_name: function.name.replace("::", "."),
                    })
                })
                .collect::<Vec<_>>();
            tests.sort_by(|left, right| left.display_name.cmp(&right.display_name));
            Ok(ProjectCompilation {
                compilation,
                sources: loader.sources,
                root_namespace,
                root_public_functions,
                root_type_names,
                manifest_paths,
                requires_application_entry,
                root_package_name,
                tests,
            })
        }
        Err(diagnostics) => {
            loader.diagnostics = diagnostics;
            Err(loader.finish_diagnostics())
        }
    }
}

fn test_source_files(project_root: &Path) -> Result<Vec<PathBuf>, Vec<ProjectDiagnostic>> {
    fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                collect(&path, files)?;
            } else if file_type.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("aster")
            {
                files.push(path);
            }
        }
        Ok(())
    }

    let directory = project_root.join("tests");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect(&directory, &mut files).map_err(|error| {
        vec![plain_error(
            &directory,
            format!("could not discover test sources: {error}"),
        )]
    })?;
    Ok(files)
}

/// A package-graph failure, reported against the manifest that caused it.
struct GraphError {
    path: PathBuf,
    message: String,
}

struct PackageGraph {
    packages: Vec<Package>,
    manifest_paths: Vec<PathBuf>,
    requires_application_entry: bool,
}

struct ResolvedPackages {
    graph: PackageGraph,
    state: ResolutionState,
}

/// Resolve the package graph from manifests and locally materialized sources.
///
/// Every path is interpreted relative to its declaring manifest. Offline mode
/// accepts only exact locked cache entries; fetch mode is the sole caller that
/// may resolve or materialize a remote Git source.
fn resolve_packages(
    root_manifest: Option<&Path>,
    project_root: &Path,
    mode: ResolutionMode<'_>,
) -> Result<ResolvedPackages, Vec<GraphError>> {
    let mut packages = vec![Package {
        name: String::new(),
        root: project_root.to_path_buf(),
        dependencies: Vec::new(),
        standard_library: false,
        immutable_source_root: None,
    }];
    let mut manifest_paths = Vec::new();

    let Some(root_manifest) = root_manifest else {
        packages.push(standard_library_package());
        return Ok(ResolvedPackages {
            graph: PackageGraph {
                packages,
                manifest_paths,
                requires_application_entry: false,
            },
            state: ResolutionState::new(mode, None),
        });
    };
    manifest_paths.push(root_manifest.to_path_buf());

    // A malformed *root* manifest stays non-fatal here: it is reported by
    // application-entry selection, which `--function` deliberately bypasses.
    // Dependency manifests have no such history and fail closed below.
    let Ok(manifest) = crate::manifest::read_manifest(root_manifest) else {
        packages.push(standard_library_package());
        return Ok(ResolvedPackages {
            graph: PackageGraph {
                packages,
                manifest_paths,
                requires_application_entry: true,
            },
            state: ResolutionState::new(mode, None),
        });
    };
    let requires_application_entry = manifest.application.is_some();
    packages[ROOT_PACKAGE]
        .name
        .clone_from(&manifest.package.name);

    let mut by_root = HashMap::new();
    by_root.insert(project_root.to_path_buf(), ROOT_PACKAGE);
    let mut by_name = HashMap::new();
    if !packages[ROOT_PACKAGE].name.is_empty() {
        by_name.insert(
            packages[ROOT_PACKAGE].name.clone(),
            project_root.to_path_buf(),
        );
    }
    let lock_path = project_root.join("Aster.lock");
    let lockfile = match crate::lockfile::read(&lock_path) {
        Ok(lockfile) => lockfile,
        Err(message) => {
            return Err(vec![GraphError {
                path: lock_path,
                message,
            }]);
        }
    };
    if lockfile.is_some() {
        manifest_paths.push(lock_path.clone());
    }
    let mut builder = GraphBuilder {
        packages,
        by_root,
        by_name,
        manifest_paths,
        stack: Vec::new(),
        errors: Vec::new(),
        mode: ResolutionState::new(mode, lockfile),
    };
    builder.resolve(ROOT_PACKAGE, root_manifest, &manifest.dependencies);
    if !builder.errors.is_empty() {
        return Err(builder.errors);
    }
    builder.finish_resolution(root_manifest);
    if !builder.errors.is_empty() {
        return Err(builder.errors);
    }
    let mut packages = builder.packages;
    packages.push(standard_library_package());
    let mut manifest_paths = builder.manifest_paths;
    manifest_paths.sort();
    manifest_paths.dedup();
    Ok(ResolvedPackages {
        graph: PackageGraph {
            packages,
            manifest_paths,
            requires_application_entry,
        },
        state: builder.mode,
    })
}

fn standard_library_package() -> Package {
    Package {
        name: String::new(),
        root: PathBuf::new(),
        dependencies: Vec::new(),
        standard_library: true,
        immutable_source_root: None,
    }
}

/// Mutable state for one package-graph walk.
struct GraphBuilder {
    packages: Vec<Package>,
    /// Deduplicates the same package reached through several graph paths.
    by_root: HashMap<PathBuf, PackageId>,
    /// Enforces one identity per declared package name.
    by_name: HashMap<String, PathBuf>,
    manifest_paths: Vec<PathBuf>,
    /// Manifests currently being resolved, for cycle detection.
    stack: Vec<PathBuf>,
    errors: Vec<GraphError>,
    mode: ResolutionState,
}

/// A dependency that passed path, manifest, and identity validation.
struct ResolvedDependency {
    root: PathBuf,
    manifest_path: PathBuf,
    manifest: crate::manifest::ProjectManifest,
    immutable_source_root: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum ResolutionMode<'a> {
    Offline,
    Fetch { update: Option<&'a str> },
}

enum ResolutionState {
    Offline {
        lockfile: Option<crate::lockfile::Lockfile>,
        used: HashSet<String>,
    },
    Fetch {
        previous: Option<crate::lockfile::Lockfile>,
        update: Option<String>,
        update_seen: bool,
        packages: Vec<crate::lockfile::LockedPackage>,
    },
}

impl ResolutionState {
    fn new(mode: ResolutionMode<'_>, lockfile: Option<crate::lockfile::Lockfile>) -> Self {
        match mode {
            ResolutionMode::Offline => Self::Offline {
                lockfile,
                used: HashSet::new(),
            },
            ResolutionMode::Fetch { update } => Self::Fetch {
                previous: lockfile,
                update: update.map(str::to_owned),
                update_seen: false,
                packages: Vec::new(),
            },
        }
    }
}

impl GraphBuilder {
    fn fail(&mut self, path: &Path, message: impl Into<String>) {
        self.errors.push(GraphError {
            path: path.to_path_buf(),
            message: message.into(),
        });
    }

    fn resolve(
        &mut self,
        owner: PackageId,
        owner_manifest: &Path,
        dependencies: &[crate::manifest::DependencyManifest],
    ) {
        if dependencies.is_empty() {
            return;
        }
        if self.packages[owner].name.is_empty() {
            self.fail(
                owner_manifest,
                "a manifest with `[dependencies]` must declare `[package] name`",
            );
            return;
        }
        let owner_directory = owner_manifest
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        self.stack.push(owner_manifest.to_path_buf());
        for dependency in dependencies {
            let Some(resolved) = self.validate(owner, owner_manifest, &owner_directory, dependency)
            else {
                continue;
            };
            self.manifest_paths.push(resolved.manifest_path.clone());
            self.by_name
                .insert(dependency.name.clone(), resolved.root.clone());
            let id = self.packages.len();
            self.packages.push(Package {
                name: dependency.name.clone(),
                root: resolved.root.clone(),
                dependencies: Vec::new(),
                standard_library: false,
                immutable_source_root: resolved.immutable_source_root,
            });
            self.by_root.insert(resolved.root, id);
            self.packages[owner].dependencies.push(id);
            self.resolve(id, &resolved.manifest_path, &resolved.manifest.dependencies);
        }
        self.stack.pop();
    }

    /// Validate one declared dependency, or record why it cannot be used.
    ///
    /// Returns `None` both for failures and for an already-resolved package,
    /// which is simply linked to the owner again.
    fn validate(
        &mut self,
        owner: PackageId,
        owner_manifest: &Path,
        owner_directory: &Path,
        dependency: &crate::manifest::DependencyManifest,
    ) -> Option<ResolvedDependency> {
        let name = &dependency.name;
        let (root, immutable_source_root) =
            self.resolve_dependency_source(owner, owner_manifest, owner_directory, dependency)?;
        if !root.is_dir() {
            self.fail(
                owner_manifest,
                format!("dependency `{name}` source is not a directory"),
            );
            return None;
        }
        let manifest_path = root.join("Aster.toml");
        if !manifest_path.is_file() {
            self.fail(
                owner_manifest,
                format!(
                    "dependency `{name}` at `{}` is not an ASTER package: no Aster.toml",
                    root.display()
                ),
            );
            return None;
        }
        if self.stack.contains(&manifest_path) {
            let mut cycle = self
                .stack
                .iter()
                .skip_while(|entry| *entry != &manifest_path)
                .map(|entry| entry.display().to_string())
                .collect::<Vec<_>>();
            cycle.push(manifest_path.display().to_string());
            self.fail(
                owner_manifest,
                format!("dependency cycle: {}", cycle.join(" -> ")),
            );
            return None;
        }
        let manifest = match crate::manifest::read_manifest(&manifest_path) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.fail(&manifest_path, error.message);
                return None;
            }
        };
        let declared_package = &manifest.package;
        if &declared_package.name != name {
            self.fail(
                owner_manifest,
                format!(
                    "dependency `{name}` resolves to a package named `{}`",
                    declared_package.name
                ),
            );
            return None;
        }
        if let Some(&existing) = self.by_root.get(&root) {
            // The same correctly named package reached twice is loaded once.
            if !self.packages[owner].dependencies.contains(&existing) {
                self.packages[owner].dependencies.push(existing);
            }
            return None;
        }
        if let Some(previous) = self.by_name.get(name)
            && previous != &root
        {
            self.fail(
                owner_manifest,
                format!(
                    "duplicate package identity `{name}`: `{}` and `{}`",
                    previous.display(),
                    root.display()
                ),
            );
            return None;
        }
        Some(ResolvedDependency {
            root,
            manifest_path,
            manifest,
            immutable_source_root,
        })
    }

    fn resolve_dependency_source(
        &mut self,
        owner: PackageId,
        owner_manifest: &Path,
        owner_directory: &Path,
        dependency: &crate::manifest::DependencyManifest,
    ) -> Option<(PathBuf, Option<PathBuf>)> {
        let name = &dependency.name;
        match &dependency.source {
            crate::manifest::DependencySource::Path { path } => {
                let requested_update = if let ResolutionState::Fetch {
                    update: Some(update),
                    update_seen,
                    ..
                } = &mut self.mode
                {
                    if update == name {
                        *update_seen = true;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if requested_update {
                    self.fail(
                        owner_manifest,
                        format!(
                            "dependency `{name}` is not a Git dependency and cannot be updated"
                        ),
                    );
                    return None;
                }
                // Resolved against the declaring manifest, never the process working
                // directory, so the graph is location-independent.
                let declared = owner_directory.join(path);
                let Ok(root) = fs::canonicalize(&declared) else {
                    self.fail(
                        owner_manifest,
                        format!(
                            "dependency `{name}` path `{path}` does not exist (resolved to `{}`)",
                            declared.display()
                        ),
                    );
                    return None;
                };
                if let Some(immutable_root) = &self.packages[owner].immutable_source_root
                    && !root.starts_with(immutable_root)
                {
                    self.fail(
                        owner_manifest,
                        format!(
                            "path dependency `{name}` escapes the immutable Git package source"
                        ),
                    );
                    return None;
                }
                Some((root, self.packages[owner].immutable_source_root.clone()))
            }
            crate::manifest::DependencySource::Git { git, rev } => {
                let root = self.resolve_git(owner_manifest, name, git, rev)?;
                Some((root.clone(), Some(root)))
            }
        }
    }

    fn resolve_git(
        &mut self,
        owner_manifest: &Path,
        name: &str,
        git: &str,
        rev: &str,
    ) -> Option<PathBuf> {
        let result = match &mut self.mode {
            ResolutionState::Offline { lockfile, used } => {
                let Some(lockfile) = lockfile else {
                    self.fail(
                        owner_manifest,
                        format!("Git dependency `{name}` is not locked; run `aster fetch`"),
                    );
                    return None;
                };
                let Some(package) = lockfile
                    .packages
                    .iter()
                    .find(|package| package.name == name)
                else {
                    self.fail(
                        owner_manifest,
                        format!("Aster.lock is stale: Git dependency `{name}` is missing; run `aster fetch`"),
                    );
                    return None;
                };
                if package.git != git || package.rev != rev {
                    self.fail(
                        owner_manifest,
                        format!(
                            "Aster.lock is stale for Git dependency `{name}`; run `aster fetch`"
                        ),
                    );
                    return None;
                }
                used.insert(name.to_owned());
                crate::git_source::cached_source(git, &package.commit)
            }
            ResolutionState::Fetch {
                previous,
                update,
                update_seen,
                packages,
            } => {
                if let Some(package) = packages.iter().find(|package| package.name == name)
                    && (package.git != git || package.rev != rev)
                {
                    self.errors.push(GraphError {
                        path: owner_manifest.to_path_buf(),
                        message: format!(
                            "Git dependency `{name}` is declared with conflicting sources"
                        ),
                    });
                    return None;
                }
                let explicitly_updated = update.as_deref() == Some(name);
                if explicitly_updated {
                    *update_seen = true;
                }
                let previous = previous.as_ref().and_then(|lockfile| {
                    lockfile.packages.iter().find(|package| {
                        package.name == name && package.git == git && package.rev == rev
                    })
                });
                let commit = if explicitly_updated {
                    crate::git_source::resolve_revision(git, rev)
                } else if let Some(previous) = previous {
                    Ok(previous.commit.clone())
                } else {
                    crate::git_source::resolve_revision(git, rev)
                };
                let commit = match commit {
                    Ok(commit) => commit,
                    Err(message) => {
                        self.errors.push(GraphError {
                            path: owner_manifest.to_path_buf(),
                            message,
                        });
                        return None;
                    }
                };
                if !packages.iter().any(|package| package.name == name) {
                    packages.push(crate::lockfile::LockedPackage {
                        name: name.to_owned(),
                        git: git.to_owned(),
                        rev: rev.to_owned(),
                        commit: commit.clone(),
                    });
                }
                crate::git_source::materialize(git, &commit)
            }
        };
        match result {
            Ok(path) => Some(path),
            Err(message) => {
                self.fail(owner_manifest, message);
                None
            }
        }
    }

    fn finish_resolution(&mut self, root_manifest: &Path) {
        match &self.mode {
            ResolutionState::Offline { lockfile, used } => {
                if let Some(extra) = lockfile.as_ref().and_then(|lockfile| {
                    lockfile
                        .packages
                        .iter()
                        .find(|package| !used.contains(&package.name))
                }) {
                    self.fail(
                        root_manifest,
                        format!(
                            "Aster.lock is stale: package `{}` is no longer in the Git dependency graph; run `aster fetch`",
                            extra.name
                        ),
                    );
                }
            }
            ResolutionState::Fetch {
                update: Some(update),
                update_seen: false,
                ..
            } => self.fail(
                root_manifest,
                format!("Git dependency `{update}` was not found"),
            ),
            ResolutionState::Fetch { .. } => {}
        }
    }
}

struct Loader {
    fallback_path: PathBuf,
    packages: Vec<Package>,
    sources: Vec<ProjectSource>,
    units: Vec<Unit>,
    loaded_files: HashSet<PathBuf>,
    loaded_namespaces: HashSet<(PackageId, String)>,
    loading_namespaces: Vec<(PackageId, String)>,
    /// Which package answered `using X` for a file in a given package. Namespace
    /// resolution happens once, here, and linking reuses the same answer.
    resolved_usings: HashMap<(PackageId, String), PackageId>,
    next_offset: usize,
    diagnostics: Vec<Diagnostic>,
    file_load_diagnostics: Vec<ProjectDiagnostic>,
    standard_library: StandardLibrary,
}

impl Loader {
    #[allow(clippy::too_many_lines)] // graph state and source diagnostics stay in one transaction
    fn load_file(
        &mut self,
        mut path: PathBuf,
        package: PackageId,
        root: bool,
        standard_library: bool,
        expected_namespace: Option<&str>,
        using_span: Option<Span>,
    ) -> Option<String> {
        if !root && !standard_library {
            let package_root = self.packages[package].root.clone();
            match fs::canonicalize(&path) {
                Ok(canonical) if canonical.starts_with(&package_root) => path = canonical,
                Ok(_) => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "a using resolved outside its package root",
                            using_span.unwrap_or_default(),
                        )
                        .with_help(
                            "keep namespace directories below the package root, and declare other packages under `[dependencies]`",
                        ),
                    );
                    return None;
                }
                Err(_) => {}
            }
        }
        if self.loaded_files.contains(&path) {
            return expected_namespace.map(str::to_owned);
        }
        let source = if standard_library {
            let namespace =
                expected_namespace.expect("standard library loads always have a namespace");
            let Some(source) = self.standard_library.source(namespace) else {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "official standard library namespace `{namespace}` is unavailable; the Aster installation is incomplete"
                        ),
                        using_span.unwrap_or_default(),
                    )
                    .with_help("reinstall Aster or restore the compiler's bundled standard library"),
                );
                return None;
            };
            source.to_owned()
        } else {
            match fs::read_to_string(&path) {
                Ok(source) => source,
                Err(error) => {
                    let diagnostic = if root {
                        Diagnostic::error(
                            format!(
                                "could not load root source file `{}`: {error}",
                                path.display()
                            ),
                            Span::default(),
                        )
                        .with_help("ensure the input source file is valid UTF-8")
                    } else {
                        Diagnostic::error(
                            format!(
                                "could not load namespace file `{}`: {error}",
                                path.display()
                            ),
                            using_span.unwrap_or_default(),
                        )
                        .with_help("create the namespace directory/file or fix the using")
                    };
                    self.file_load_diagnostics.push(ProjectDiagnostic {
                        path: path.clone(),
                        source: String::new(),
                        diagnostic,
                    });
                    return None;
                }
            }
        };
        let offset = self.next_offset;
        self.next_offset = self.next_offset.saturating_add(source.len() + 1);
        self.sources.push(ProjectSource {
            path: path.clone(),
            source: source.clone(),
            offset,
            origin: if standard_library {
                ProjectSourceOrigin::StandardLibrary
            } else {
                ProjectSourceOrigin::Project
            },
        });
        let tokens = match lex(&source) {
            Ok(mut tokens) => {
                for token in &mut tokens {
                    token.span.start = token.span.start.saturating_add(offset);
                    token.span.end = token.span.end.saturating_add(offset);
                }
                tokens
            }
            Err(mut diagnostics) => {
                for diagnostic in &mut diagnostics {
                    diagnostic.span.start = diagnostic.span.start.saturating_add(offset);
                    diagnostic.span.end = diagnostic.span.end.saturating_add(offset);
                }
                self.diagnostics.append(&mut diagnostics);
                return None;
            }
        };
        let module = match parse(tokens.clone()) {
            Ok(module) => module,
            Err(mut diagnostics) => {
                self.diagnostics.append(&mut diagnostics);
                return None;
            }
        };
        let inferred = if standard_library {
            expected_namespace.unwrap_or_default().to_owned()
        } else {
            match self.inferred_namespace(package, &path) {
                Ok(namespace) => namespace,
                Err(message) => {
                    self.diagnostics.push(
                        Diagnostic::error(message, using_span.unwrap_or_default())
                            .with_help("keep source files below the project root"),
                    );
                    return None;
                }
            }
        };
        let declared = module.namespace.as_ref().map(|value| value.name.as_str());
        if let Some(declared) = declared
            && declared != inferred
        {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "namespace `{declared}` does not match the directory namespace `{}`",
                        display_namespace(&inferred)
                    ),
                    module
                        .namespace
                        .as_ref()
                        .map_or_else(Span::default, |value| value.span),
                )
                .with_help(format!(
                    "write `namespace {};` or move the file to the matching directory",
                    display_namespace(&inferred)
                )),
            );
            return None;
        }
        let namespace = declared.unwrap_or(&inferred).to_owned();
        if !standard_library && is_official_name(&namespace) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "namespace `{namespace}` uses the reserved `aster.*` standard-library namespace"
                    ),
                    module
                        .namespace
                        .as_ref()
                        .map_or_else(Span::default, |value| value.span),
                )
                .with_help("rename the project namespace; official `aster.*` namespaces cannot be replaced"),
            );
            return None;
        }
        self.loaded_files.insert(path.clone());
        let unit_index = self.units.len();
        self.units.push(Unit {
            path,
            name: namespace.clone(),
            package,
            root,
            standard_library,
            tokens,
            module,
        });

        let usings = self.units[unit_index].module.usings.clone();
        let mut seen = HashSet::new();
        for using in usings {
            if standard_library && !is_official_name(&using.name) {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "standard library namespace `{namespace}` cannot use project namespace `{}`",
                            using.name
                        ),
                        using.span,
                    )
                    .with_help("standard-library namespaces may use only other `aster.*` namespaces"),
                );
                continue;
            }
            if !seen.insert(using.name.clone()) {
                self.diagnostics.push(
                    Diagnostic::error(format!("duplicate using `{}`", using.name), using.span)
                        .with_help("remove the repeated using"),
                );
                continue;
            }
            self.load_namespace(&using.name, package, Some(using.span));
        }
        Some(namespace)
    }

    fn namespace_path(&self, package: PackageId, name: &str) -> PathBuf {
        let mut path = self.packages[package].root.clone();
        for segment in name.split('.') {
            if !segment.is_empty() {
                path.push(segment);
            }
        }
        path
    }

    fn inferred_namespace(&self, package: PackageId, path: &Path) -> Result<String, String> {
        let package_root = &self.packages[package].root;
        let parent = path
            .parent()
            .ok_or_else(|| format!("source file `{}` has no parent directory", path.display()))?;
        let relative = parent.strip_prefix(package_root).map_err(|_| {
            format!(
                "source file `{}` is outside package root `{}`",
                path.display(),
                package_root.display()
            )
        })?;
        let mut segments = Vec::new();
        for component in relative.components() {
            let std::path::Component::Normal(segment) = component else {
                return Err("namespace path contains an unsupported component".to_owned());
            };
            let segment = segment
                .to_str()
                .ok_or_else(|| "namespace path is not valid Unicode".to_owned())?;
            if !valid_namespace_segment(segment) {
                return Err(format!(
                    "directory `{segment}` cannot be used as an Aster namespace segment"
                ));
            }
            segments.push(segment);
        }
        Ok(segments.join("."))
    }

    /// Namespace files that a package provides directly, in stable order.
    fn namespace_files(&self, package: PackageId, name: &str) -> Vec<PathBuf> {
        let directory = self.namespace_path(package, name);
        let Ok(entries) = fs::read_dir(&directory) else {
            return Vec::new();
        };
        let mut files = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("aster")
            })
            .collect::<Vec<_>>();
        // Directory enumeration order is not stable across platforms.
        files.sort();
        files
    }

    /// Which package answers `using name` for a file in `from`: the package
    /// itself, or one of its direct dependencies. A dependency is reachable
    /// only when it was declared, so this never becomes filesystem traversal.
    fn using_owner(
        &mut self,
        name: &str,
        from: PackageId,
        using_span: Option<Span>,
    ) -> Option<PackageId> {
        // A package's own namespaces take precedence over a dependency's,
        // matching the existing rule that local declarations shadow imported
        // ones. Adding a dependency therefore cannot capture a `using` that
        // already resolved inside the package.
        if !self.namespace_files(from, name).is_empty() {
            return Some(from);
        }
        let matches = self.packages[from]
            .dependencies
            .clone()
            .into_iter()
            .filter(|candidate| !self.namespace_files(*candidate, name).is_empty())
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => {
                let directory = self.namespace_path(from, name);
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "namespace `{}` was not found in package `{}` or its dependencies",
                            display_namespace(name),
                            display_package(&self.packages[from])
                        ),
                        using_span.unwrap_or_default(),
                    )
                    .with_help(format!(
                        "create `{}` with at least one `.aster` file, or declare the providing package under `[dependencies]`",
                        directory.display()
                    )),
                );
                None
            }
            [single] => Some(*single),
            multiple => {
                let owners = multiple
                    .iter()
                    .map(|package| display_package(&self.packages[*package]))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "namespace `{}` is provided by more than one package: {owners}",
                            display_namespace(name)
                        ),
                        using_span.unwrap_or_default(),
                    )
                    .with_help("rename one namespace; ASTER has no package-qualified using yet"),
                );
                None
            }
        }
    }

    fn load_namespace(&mut self, name: &str, from: PackageId, using_span: Option<Span>) {
        if is_official_name(name) {
            self.load_standard_library_namespace(name, using_span);
            return;
        }
        let Some(owner) = self.using_owner(name, from, using_span) else {
            return;
        };
        self.resolved_usings.insert((from, name.to_owned()), owner);
        let key = (owner, name.to_owned());
        if self.loaded_namespaces.contains(&key) {
            return;
        }
        if self.loading_namespaces.last() == Some(&key) {
            return;
        }
        if let Some(position) = self
            .loading_namespaces
            .iter()
            .position(|value| value == &key)
        {
            let mut cycle = self.loading_namespaces[position..]
                .iter()
                .map(|(_, value)| display_namespace(value).to_owned())
                .collect::<Vec<_>>();
            cycle.push(display_namespace(name).to_owned());
            self.diagnostics.push(
                Diagnostic::error(
                    format!("circular using: {}", cycle.join(" -> ")),
                    using_span.unwrap_or_default(),
                )
                .with_help("remove one using from the namespace cycle"),
            );
            return;
        }
        self.loading_namespaces.push(key.clone());
        for file in self.namespace_files(owner, name) {
            self.load_file(file, owner, false, false, Some(name), using_span);
        }
        self.loading_namespaces.pop();
        self.loaded_namespaces.insert(key);
    }

    fn load_standard_library_namespace(&mut self, name: &str, using_span: Option<Span>) {
        let stdlib = self.standard_library_package();
        let key = (stdlib, name.to_owned());
        if self.loaded_namespaces.contains(&key) || self.loading_namespaces.last() == Some(&key) {
            return;
        }
        self.loading_namespaces.push(key.clone());
        self.load_file(
            StandardLibrary::display_path(name),
            stdlib,
            false,
            true,
            Some(name),
            using_span,
        );
        self.loading_namespaces.pop();
        self.loaded_namespaces.insert(key);
    }

    fn standard_library_package(&self) -> PackageId {
        self.packages
            .iter()
            .position(|package| package.standard_library)
            .expect("the graph always contains the standard-library package")
    }

    fn finish_diagnostics(&self) -> Vec<ProjectDiagnostic> {
        let mut diagnostics = self
            .diagnostics
            .iter()
            .cloned()
            .map(|mut diagnostic| {
                let source = self
                    .sources
                    .iter()
                    .filter(|source| diagnostic.span.start >= source.offset)
                    .max_by_key(|source| source.offset)
                    .or_else(|| self.sources.first());
                let Some(source) = source else {
                    return ProjectDiagnostic {
                        path: self.fallback_path.clone(),
                        source: String::new(),
                        diagnostic,
                    };
                };
                diagnostic.span.start = diagnostic.span.start.saturating_sub(source.offset);
                diagnostic.span.end = diagnostic.span.end.saturating_sub(source.offset);
                ProjectDiagnostic {
                    path: source.path.clone(),
                    source: source.source.clone(),
                    diagnostic,
                }
            })
            .collect::<Vec<_>>();
        diagnostics.extend(self.file_load_diagnostics.clone());
        diagnostics
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| format!("could not determine the current directory: {error}"))?
    };
    fs::canonicalize(&absolute)
        .map_err(|error| format!("could not read `{}`: {error}", absolute.display()))
}

fn plain_error(path: &Path, message: impl Into<String>) -> ProjectDiagnostic {
    ProjectDiagnostic {
        path: path.to_path_buf(),
        source: String::new(),
        diagnostic: Diagnostic::error(message, Span::default()),
    }
}

fn display_namespace(name: &str) -> &str {
    if name.is_empty() { "<global>" } else { name }
}

fn display_package(package: &Package) -> &str {
    if package.standard_library {
        "aster"
    } else if package.name.is_empty() {
        "<root>"
    } else {
        &package.name
    }
}

fn valid_namespace_segment(segment: &str) -> bool {
    let mut characters = segment.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_alphabetic())
        && characters.all(|character| character == '_' || character.is_alphanumeric())
}

/// The compiler-internal nominal identity of one declaration.
///
/// A package's identity is its declared `[package] name`, independent of
/// whether that package is the graph root, a direct dependency, or a
/// transitive one: any unit whose package has a name gets that name prefixed
/// ahead of its namespace. A manifest-less root has an implicit empty package
/// identity and keeps the direct-file bare/namespace-only scheme. Package
/// *names* participate here; filesystem paths never do.
///
fn linked_name(unit: &Unit, packages: &[Package], name: &str) -> String {
    let package = &packages[unit.package];
    if unit.root && package.name.is_empty() {
        return name.to_owned();
    }
    let namespace_scoped = namespace_scoped_name(&unit.name, name);
    if unit.standard_library || package.name.is_empty() {
        return namespace_scoped;
    }
    format!("{}::{namespace_scoped}", package.name)
}

fn linked_root_type_names(root: Option<&Unit>, packages: &[Package]) -> HashSet<String> {
    root.into_iter()
        .flat_map(|unit| {
            unit.module.items.iter().filter_map(|item| {
                let name = match item {
                    Item::Class(value) | Item::Struct(value) | Item::Interface(value) => {
                        &value.name
                    }
                    Item::Enum(value) => &value.name,
                    Item::Function(_) | Item::Variable(_) => return None,
                };
                Some(linked_name(unit, packages, name))
            })
        })
        .collect()
}

fn namespace_scoped_name(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        format!("<global>::{name}")
    } else {
        format!("{namespace}::{name}")
    }
}

#[allow(clippy::too_many_lines)] // builds one deterministic linked unit in a single pass
fn link(
    units: &[Unit],
    packages: &[Package],
    resolved_usings: &HashMap<(PackageId, String), PackageId>,
) -> (Module, Vec<Token>, Vec<Diagnostic>) {
    type SymbolTable = HashMap<(PackageId, String), HashMap<String, String>>;
    let mut namespace_symbols: SymbolTable = HashMap::new();
    let mut public_symbols: SymbolTable = HashMap::new();
    let mut internal_symbols: SymbolTable = HashMap::new();
    // Cross-package collisions are a package-model error, so they are detected
    // on the fully linked identity rather than being silently merged.
    let mut owners: HashMap<String, PackageId> = HashMap::new();
    let mut collisions: Vec<(String, PackageId, PackageId)> = Vec::new();
    for unit in units {
        for item in &unit.module.items {
            let (name, visibility) = item_name_visibility(item);
            let linked = linked_name(unit, packages, name);
            if let Some(previous) = owners.insert(linked.clone(), unit.package)
                && previous != unit.package
            {
                collisions.push((linked.clone(), previous, unit.package));
            }
            let key = (unit.package, unit.name.clone());
            namespace_symbols
                .entry(key.clone())
                .or_default()
                .insert(name.to_owned(), linked.clone());
            if visibility == Visibility::Public {
                public_symbols
                    .entry(key)
                    .or_default()
                    .insert(name.to_owned(), linked);
            } else if !unit.standard_library && visibility == Visibility::Internal {
                internal_symbols
                    .entry(key)
                    .or_default()
                    .insert(name.to_owned(), linked);
            }
        }
    }

    let mut items = Vec::new();
    let mut tokens = Vec::new();
    let mut diagnostics = Vec::new();
    for (linked, left, right) in collisions {
        diagnostics.push(
            Diagnostic::error(
                format!(
                    "declaration `{linked}` is provided by more than one package: `{}` and `{}`",
                    display_package(&packages[left]),
                    display_package(&packages[right])
                ),
                Span::default(),
            )
            .with_help(
                "rename the namespace or declaration so each package keeps a distinct identity",
            ),
        );
    }
    for unit in units {
        let empty_visible = HashMap::new();
        let local_visible = namespace_symbols
            .get(&(unit.package, unit.name.clone()))
            .unwrap_or(&empty_visible);
        let mut imported_visible = HashMap::new();
        let mut inaccessible = HashMap::new();
        let mut imported_from: HashMap<String, String> = HashMap::new();
        for using in &unit.module.usings {
            let owner = if is_official_name(&using.name) {
                units
                    .iter()
                    .find(|candidate| candidate.standard_library && candidate.name == using.name)
                    .map(|candidate| candidate.package)
            } else {
                resolved_usings
                    .get(&(unit.package, using.name.clone()))
                    .copied()
            };
            let Some(owner) = owner else {
                continue;
            };
            let key = (owner, using.name.clone());
            for imported in units
                .iter()
                .filter(|candidate| candidate.package == owner && candidate.name == using.name)
            {
                for item in &imported.module.items {
                    let (name, visibility) = item_name_visibility(item);
                    // Non-public declarations become an explicit "not
                    // accessible" answer across the standard library and across
                    // any package boundary, instead of silently not resolving.
                    let hidden = if imported.standard_library {
                        visibility != Visibility::Public
                    } else {
                        owner != unit.package && visibility != Visibility::Public
                    };
                    if hidden {
                        inaccessible.insert(
                            name.to_owned(),
                            InaccessibleOrigin {
                                namespace: using.name.clone(),
                                package: display_package(&packages[owner]).to_owned(),
                                cross_package: !imported.standard_library && owner != unit.package,
                            },
                        );
                    }
                }
            }
            // `internal` crosses namespaces but never packages.
            let mut importable = public_symbols.get(&key).cloned().unwrap_or_default();
            if owner == unit.package {
                importable.extend(internal_symbols.get(&key).cloned().unwrap_or_default());
            }
            for (name, linked) in &importable {
                if local_visible.contains_key(name) {
                    if units
                        .iter()
                        .any(|unit| unit.name == using.name && unit.standard_library)
                    {
                        diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "local declaration `{name}` conflicts with the official export from `{}`",
                                    using.name
                                ),
                                using.span,
                            )
                            .with_help(
                                "rename the local declaration; standard-library exports cannot be shadowed",
                            ),
                        );
                    }
                    continue;
                }
                if let Some(previous) = imported_from.insert(name.clone(), using.name.clone()) {
                    if previous != using.name {
                        diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "name `{name}` is ambiguous between namespaces `{previous}` and `{}`",
                                    using.name
                                ),
                                using.span,
                            )
                            .with_help("reorganize or rename one declaration; using aliases and qualified references are not implemented yet"),
                        );
                    }
                } else {
                    imported_visible.insert(name.clone(), linked.clone());
                }
            }
        }
        let mut rewriter = Rewriter {
            local_visible,
            imported_visible,
            inaccessible,
            diagnostics: Vec::new(),
            type_parameters: HashSet::new(),
            locals: Vec::new(),
            members: HashSet::new(),
        };
        for mut item in unit.module.items.clone() {
            rewriter.visit_item_mut(&mut item);
            items.push(item);
        }
        diagnostics.append(&mut rewriter.diagnostics);
        tokens.extend(
            unit.tokens
                .iter()
                .filter(|token| !matches!(token.kind, aster_syntax::TokenKind::Eof))
                .cloned(),
        );
    }
    if let Some(last) = units.last() {
        let end = last.tokens.last().map_or(0, |token| token.span.end);
        tokens.push(Token {
            kind: aster_syntax::TokenKind::Eof,
            span: Span::new(end, end),
        });
    }
    (
        Module {
            namespace: None,
            usings: Vec::new(),
            items,
        },
        tokens,
        diagnostics,
    )
}

fn item_name_visibility(item: &Item) -> (&str, Visibility) {
    match item {
        Item::Class(value) | Item::Struct(value) | Item::Interface(value) => {
            (&value.name, value.visibility)
        }
        Item::Enum(value) => (&value.name, value.visibility),
        Item::Function(value) => (&value.name, value.visibility),
        Item::Variable(value) => (
            &value.name,
            value.visibility.unwrap_or(Visibility::Internal),
        ),
    }
}

/// Why a name resolved but is not usable here.
#[derive(Clone, Debug)]
struct InaccessibleOrigin {
    namespace: String,
    package: String,
    cross_package: bool,
}

impl InaccessibleOrigin {
    fn describe(&self, name: &str) -> (String, &'static str) {
        if self.cross_package {
            (
                format!(
                    "`{name}` is internal to package `{}` and is not part of its public API",
                    self.package
                ),
                "only `public` declarations cross a package dependency boundary",
            )
        } else {
            (
                format!("`{name}` is internal to namespace `{}`", self.namespace),
                "only public declarations are accessible from that namespace",
            )
        }
    }
}

struct Rewriter<'a> {
    local_visible: &'a HashMap<String, String>,
    imported_visible: HashMap<String, String>,
    inaccessible: HashMap<String, InaccessibleOrigin>,
    diagnostics: Vec<Diagnostic>,
    type_parameters: HashSet<String>,
    locals: Vec<HashSet<String>>,
    members: HashSet<String>,
}

impl Rewriter<'_> {
    fn visible(&self, name: &str) -> Option<&String> {
        self.local_visible
            .get(name)
            .or_else(|| self.imported_visible.get(name))
    }

    fn current_locals(&self) -> Option<&HashSet<String>> {
        self.locals.last()
    }

    fn rewrite_type_name(&mut self, name: &str, span: Span) -> String {
        let Some(mut type_name) = crate::type_names::TypeName::parse(name) else {
            return name.to_owned();
        };
        type_name.map_names(&mut |base| {
            if self.type_parameters.contains(base) {
                return base.to_owned();
            }
            if let Some(linked) = self.visible(base) {
                linked.clone()
            } else if let Some(origin) = self.inaccessible.get(base) {
                let (message, help) = origin.describe(base);
                self.diagnostics
                    .push(Diagnostic::error(message, span).with_help(help));
                base.to_owned()
            } else {
                base.to_owned()
            }
        });
        type_name.to_string()
    }
}

impl AstVisitorMut for Rewriter<'_> {
    fn visit_item_mut(&mut self, item: &mut Item) {
        match item {
            Item::Class(value) | Item::Struct(value) | Item::Interface(value) => {
                let previous_parameters = std::mem::replace(
                    &mut self.type_parameters,
                    value
                        .type_parameters
                        .iter()
                        .map(|parameter| parameter.name.clone())
                        .collect(),
                );
                let previous_members = std::mem::replace(
                    &mut self.members,
                    value
                        .members
                        .iter()
                        .map(|member| match member {
                            Member::Field(field) => field.name.clone(),
                            Member::Method(method) => method.name.clone(),
                            Member::Property(property) => property.name.clone(),
                        })
                        .collect(),
                );
                let original = value.name.clone();
                for member in &mut value.members {
                    if let Member::Method(method) = member
                        && method.constructor
                    {
                        method
                            .name
                            .clone_from(self.visible(&original).expect("declared type is visible"));
                    }
                }
                walk_type_declaration_mut(self, value);
                value
                    .name
                    .clone_from(self.visible(&original).expect("declared type is visible"));
                self.members = previous_members;
                self.type_parameters = previous_parameters;
            }
            Item::Enum(value) => {
                let previous_parameters = std::mem::replace(
                    &mut self.type_parameters,
                    value
                        .type_parameters
                        .iter()
                        .map(|parameter| parameter.name.clone())
                        .collect(),
                );
                let original = value.name.clone();
                self.visit_enum_declaration_mut(value);
                value
                    .name
                    .clone_from(self.visible(&original).expect("declared enum is visible"));
                self.type_parameters = previous_parameters;
            }
            Item::Function(value) => {
                let original = value.name.clone();
                self.visit_function_declaration_mut(value);
                value.name.clone_from(
                    self.visible(&original)
                        .expect("declared function is visible"),
                );
            }
            Item::Variable(value) => {
                let original = value.name.clone();
                self.visit_variable_declaration_mut(value);
                value.name.clone_from(
                    self.visible(&original)
                        .expect("declared variable is visible"),
                );
            }
        }
    }

    fn visit_type_declaration_mut(&mut self, declaration: &mut TypeDeclaration) {
        walk_type_declaration_mut(self, declaration);
    }

    fn visit_function_declaration_mut(&mut self, declaration: &mut FunctionDeclaration) {
        let previous_parameters = self.type_parameters.clone();
        self.type_parameters.extend(
            declaration
                .type_parameters
                .iter()
                .map(|parameter| parameter.name.clone()),
        );
        let locals = declaration
            .parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect();
        self.locals.push(locals);
        walk_function_declaration_mut(self, declaration);
        self.locals.pop();
        self.type_parameters = previous_parameters;
    }

    fn visit_property_mut(&mut self, property: &mut Property) {
        self.visit_type_ref_mut(&mut property.type_ref);
        if let Some(getter) = &mut property.getter {
            self.visit_accessor_mut(getter);
        }
        if let Some(setter) = &mut property.setter {
            let mut locals = self.current_locals().cloned().unwrap_or_default();
            locals.insert("value".to_owned());
            self.locals.push(locals);
            self.visit_accessor_mut(setter);
            self.locals.pop();
        }
    }

    fn visit_block_mut(&mut self, block: &mut aster_syntax::Block) {
        let locals = self.current_locals().cloned().unwrap_or_default();
        self.locals.push(locals);
        aster_syntax::visit::walk_block_mut(self, block);
        self.locals.pop();
    }

    fn visit_statement_mut(&mut self, statement: &mut Statement) {
        if let Statement::ForEach { element_name, .. } = statement {
            let mut locals = self.current_locals().cloned().unwrap_or_default();
            locals.insert(element_name.clone());
            self.locals.push(locals);
            walk_statement_mut(self, statement);
            self.locals.pop();
        } else if matches!(statement, Statement::For { .. }) {
            let locals = self.current_locals().cloned().unwrap_or_default();
            self.locals.push(locals);
            walk_statement_mut(self, statement);
            self.locals.pop();
        } else {
            walk_statement_mut(self, statement);
        }
    }

    fn visit_switch_case_mut(&mut self, case: &mut SwitchCase) {
        if let Some(owner) = &mut case.enum_name
            && let Some(name) = self.visible(owner)
        {
            owner.clone_from(name);
        }
        let mut locals = self.current_locals().cloned().unwrap_or_default();
        locals.extend(case.bindings.iter().cloned());
        self.locals.push(locals);
        walk_switch_case_mut(self, case);
        self.locals.pop();
    }

    fn visit_variable_declaration_mut(&mut self, declaration: &mut VariableDeclaration) {
        walk_variable_declaration_mut(self, declaration);
        if let Some(locals) = self.locals.last_mut() {
            locals.insert(declaration.name.clone());
        }
    }

    fn visit_expression_mut(&mut self, expression: &mut Expression) {
        match &mut expression.kind {
            ExpressionKind::Name(name) => {
                let local = self
                    .current_locals()
                    .is_some_and(|locals| locals.contains(name));
                if !local
                    && crate::type_names::TypeName::parse(name)
                        .is_some_and(|type_name| !type_name.arguments.is_empty())
                {
                    *name = self.rewrite_type_name(name, expression.span);
                } else if !local
                    && !self.members.contains(name)
                    && let Some(linked) = self.visible(name)
                {
                    *name = linked.clone();
                } else if !local
                    && !self.members.contains(name)
                    && let Some(origin) = self.inaccessible.get(name)
                {
                    let (message, help) = origin.describe(name);
                    self.diagnostics
                        .push(Diagnostic::error(message, expression.span).with_help(help));
                }
            }
            ExpressionKind::StructLiteral { type_name, .. }
            | ExpressionKind::NewObject {
                type_name: Some(type_name),
                ..
            } => {
                *type_name = self.rewrite_type_name(type_name, expression.span);
            }
            _ => {}
        }
        walk_expression_mut(self, expression);
    }

    fn visit_type_ref_mut(&mut self, type_ref: &mut TypeRef) {
        type_ref.name = self.rewrite_type_name(&type_ref.name, type_ref.span);
    }
}

#[cfg(test)]
mod standard_library_tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{StandardLibrary, compile_project_with_standard_library};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn missing_official_namespace_reports_an_incomplete_distribution() {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aster-missing-stdlib-{}-{id}.aster",
            std::process::id()
        ));
        fs::write(&path, "using aster.math; public int Run() { return 0; }")
            .expect("write test source");
        let diagnostics = compile_project_with_standard_library(&path, StandardLibrary::empty())
            .expect_err("missing embedded standard library must fail");
        fs::remove_file(path).expect("remove test source");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .diagnostic
                .message
                .contains("installation is incomplete")
        }));
    }
}
