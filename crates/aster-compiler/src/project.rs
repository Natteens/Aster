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
}

impl ProjectCompilation {
    #[must_use]
    pub fn dependency_paths(&self) -> Vec<PathBuf> {
        self.sources
            .iter()
            .filter(|source| source.origin == ProjectSourceOrigin::Project)
            .map(|source| source.path.clone())
            .collect()
    }

    #[must_use]
    pub fn is_root_public_function(&self, name: &str) -> bool {
        self.root_public_functions.contains(name)
    }
}

#[derive(Clone)]
struct Unit {
    name: String,
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

fn compile_project_with_standard_library(
    path: &Path,
    standard_library: StandardLibrary,
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
    let mut loader = Loader {
        fallback_path: root_path.clone(),
        project_root,
        sources: Vec::new(),
        units: Vec::new(),
        loaded_files: HashSet::new(),
        loaded_namespaces: HashSet::new(),
        loading_namespaces: Vec::new(),
        next_offset: 0,
        diagnostics: Vec::new(),
        standard_library,
    };
    let root_namespace = loader
        .load_file(root_path, true, false, None, None)
        .unwrap_or_default();
    if manifest.is_some()
        && !loader
            .diagnostics
            .iter()
            .any(|value| value.severity == aster_diagnostics::Severity::Error)
    {
        loader.load_namespace(&root_namespace, None);
    }
    if !loader.diagnostics.is_empty() {
        return Err(loader.finish_diagnostics());
    }
    let root_public_functions = loader
        .units
        .iter()
        .find(|unit| unit.root)
        .into_iter()
        .flat_map(|unit| &unit.module.items)
        .filter_map(|item| {
            let Item::Function(function) = item else {
                return None;
            };
            (function.visibility == Visibility::Public).then(|| function.name.clone())
        })
        .collect();
    let (module, tokens, mut link_diagnostics) = link(&loader.units);
    if !link_diagnostics.is_empty() {
        loader.diagnostics.append(&mut link_diagnostics);
        return Err(loader.finish_diagnostics());
    }
    let intrinsic_bindings = loader.standard_library.intrinsic_bindings();
    match compile_module(tokens, module, &intrinsic_bindings) {
        Ok(compilation) => Ok(ProjectCompilation {
            compilation,
            sources: loader.sources,
            root_namespace,
            root_public_functions,
        }),
        Err(diagnostics) => {
            loader.diagnostics = diagnostics;
            Err(loader.finish_diagnostics())
        }
    }
}

struct Loader {
    fallback_path: PathBuf,
    project_root: PathBuf,
    sources: Vec<ProjectSource>,
    units: Vec<Unit>,
    loaded_files: HashSet<PathBuf>,
    loaded_namespaces: HashSet<String>,
    loading_namespaces: Vec<String>,
    next_offset: usize,
    diagnostics: Vec<Diagnostic>,
    standard_library: StandardLibrary,
}

impl Loader {
    #[allow(clippy::too_many_lines)] // graph state and source diagnostics stay in one transaction
    fn load_file(
        &mut self,
        mut path: PathBuf,
        root: bool,
        standard_library: bool,
        expected_namespace: Option<&str>,
        using_span: Option<Span>,
    ) -> Option<String> {
        if !root && !standard_library {
            match fs::canonicalize(&path) {
                Ok(canonical) if canonical.starts_with(&self.project_root) => path = canonical,
                Ok(_) => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "a using resolved outside the project root",
                            using_span.unwrap_or_default(),
                        )
                        .with_help("keep namespace directories below the project root"),
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
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "could not load namespace file `{}`: {error}",
                                path.display()
                            ),
                            using_span.unwrap_or_default(),
                        )
                        .with_help("create the namespace directory/file or fix the using"),
                    );
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
        let padded = format!("{}{}", " ".repeat(offset), source);
        let tokens = match lex(&padded) {
            Ok(tokens) => tokens,
            Err(mut diagnostics) => {
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
            match self.inferred_namespace(&path) {
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
            name: namespace.clone(),
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
            self.load_namespace(&using.name, Some(using.span));
        }
        Some(namespace)
    }

    fn namespace_path(&self, name: &str) -> PathBuf {
        let mut path = self.project_root.clone();
        for segment in name.split('.') {
            if !segment.is_empty() {
                path.push(segment);
            }
        }
        path
    }

    fn inferred_namespace(&self, path: &Path) -> Result<String, String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("source file `{}` has no parent directory", path.display()))?;
        let relative = parent.strip_prefix(&self.project_root).map_err(|_| {
            format!(
                "source file `{}` is outside project root `{}`",
                path.display(),
                self.project_root.display()
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

    fn load_namespace(&mut self, name: &str, using_span: Option<Span>) {
        if self.loaded_namespaces.contains(name) {
            return;
        }
        if self
            .loading_namespaces
            .last()
            .is_some_and(|value| value == name)
        {
            return;
        }
        if let Some(position) = self
            .loading_namespaces
            .iter()
            .position(|value| value == name)
        {
            let mut cycle = self.loading_namespaces[position..].to_vec();
            cycle.push(name.to_owned());
            self.diagnostics.push(
                Diagnostic::error(
                    format!("circular using: {}", cycle.join(" -> ")),
                    using_span.unwrap_or_default(),
                )
                .with_help("remove one using from the namespace cycle"),
            );
            return;
        }
        self.loading_namespaces.push(name.to_owned());
        if is_official_name(name) {
            self.load_file(
                StandardLibrary::display_path(name),
                false,
                true,
                Some(name),
                using_span,
            );
        } else {
            let directory = self.namespace_path(name);
            let mut files = match fs::read_dir(&directory) {
                Ok(entries) => entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.is_file()
                            && path.extension().and_then(|value| value.to_str()) == Some("aster")
                    })
                    .collect::<Vec<_>>(),
                Err(error) => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "namespace `{}` was not found at `{}`: {error}",
                                display_namespace(name),
                                directory.display()
                            ),
                            using_span.unwrap_or_default(),
                        )
                        .with_help("create the namespace directory with at least one `.aster` file or fix the using"),
                    );
                    self.loading_namespaces.pop();
                    return;
                }
            };
            files.sort();
            if files.is_empty() {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "namespace `{}` has no direct `.aster` files in `{}`",
                            display_namespace(name),
                            directory.display()
                        ),
                        using_span.unwrap_or_default(),
                    )
                    .with_help("add an `.aster` file directly to the namespace directory"),
                );
            }
            for file in files {
                self.load_file(file, false, false, Some(name), using_span);
            }
        }
        self.loading_namespaces.pop();
        self.loaded_namespaces.insert(name.to_owned());
    }

    fn finish_diagnostics(&self) -> Vec<ProjectDiagnostic> {
        self.diagnostics
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
            .collect()
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

fn valid_namespace_segment(segment: &str) -> bool {
    let mut characters = segment.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_alphabetic())
        && characters.all(|character| character == '_' || character.is_alphanumeric())
}

#[allow(clippy::too_many_lines)] // builds one deterministic linked unit in a single pass
fn link(units: &[Unit]) -> (Module, Vec<Token>, Vec<Diagnostic>) {
    let mut namespace_symbols: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut usable_symbols: HashMap<String, HashMap<String, String>> = HashMap::new();
    for unit in units {
        for item in &unit.module.items {
            let (name, visibility) = item_name_visibility(item);
            let linked = if unit.root {
                name.to_owned()
            } else if unit.name.is_empty() {
                format!("<global>::{name}")
            } else {
                format!("{}::{name}", unit.name)
            };
            namespace_symbols
                .entry(unit.name.clone())
                .or_default()
                .insert(name.to_owned(), linked.clone());
            if visibility == Visibility::Public
                || (!unit.standard_library && visibility == Visibility::Internal)
            {
                usable_symbols
                    .entry(unit.name.clone())
                    .or_default()
                    .insert(name.to_owned(), linked);
            }
        }
    }

    let mut items = Vec::new();
    let mut tokens = Vec::new();
    let mut diagnostics = Vec::new();
    for unit in units {
        let mut visible = namespace_symbols
            .get(&unit.name)
            .cloned()
            .unwrap_or_default();
        let mut inaccessible = HashMap::new();
        let local_names: HashSet<_> = visible.keys().cloned().collect();
        let mut imported_from: HashMap<String, String> = HashMap::new();
        for using in &unit.module.usings {
            for imported in units
                .iter()
                .filter(|candidate| candidate.name == using.name)
            {
                for item in &imported.module.items {
                    let (name, visibility) = item_name_visibility(item);
                    if imported.standard_library && visibility != Visibility::Public {
                        inaccessible.insert(name.to_owned(), using.name.clone());
                    }
                }
            }
            if let Some(names) = usable_symbols.get(&using.name) {
                for (name, linked) in names {
                    if local_names.contains(name) {
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
                        visible.insert(name.clone(), linked.clone());
                    }
                }
            }
        }
        let mut rewriter = Rewriter {
            visible,
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

struct Rewriter {
    visible: HashMap<String, String>,
    inaccessible: HashMap<String, String>,
    diagnostics: Vec<Diagnostic>,
    type_parameters: HashSet<String>,
    locals: Vec<HashSet<String>>,
    members: HashSet<String>,
}

impl Rewriter {
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
            if let Some(linked) = self.visible.get(base) {
                linked.clone()
            } else if let Some(namespace) = self.inaccessible.get(base) {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("type `{base}` is internal to namespace `{namespace}`"),
                        span,
                    )
                    .with_help("only public types are accessible from that namespace"),
                );
                base.to_owned()
            } else {
                base.to_owned()
            }
        });
        type_name.to_string()
    }
}

impl AstVisitorMut for Rewriter {
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
                        method.name.clone_from(&self.visible[&original]);
                    }
                }
                walk_type_declaration_mut(self, value);
                value.name.clone_from(&self.visible[&original]);
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
                value.name.clone_from(&self.visible[&original]);
                self.type_parameters = previous_parameters;
            }
            Item::Function(value) => {
                let original = value.name.clone();
                self.visit_function_declaration_mut(value);
                value.name.clone_from(&self.visible[&original]);
            }
            Item::Variable(value) => {
                let original = value.name.clone();
                self.visit_variable_declaration_mut(value);
                value.name.clone_from(&self.visible[&original]);
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
        if matches!(statement, Statement::For { .. }) {
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
            && let Some(name) = self.visible.get(owner)
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
                    && let Some(linked) = self.visible.get(name)
                {
                    *name = linked.clone();
                } else if !local
                    && !self.members.contains(name)
                    && let Some(module) = self.inaccessible.get(name)
                {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("`{name}` is internal to namespace `{module}`"),
                            expression.span,
                        )
                        .with_help("only public declarations are accessible from that namespace"),
                    );
                }
            }
            ExpressionKind::StructLiteral { type_name, .. }
            | ExpressionKind::NewObject { type_name, .. } => {
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
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{StandardLibrary, compile_project_with_standard_library};

    #[test]
    fn missing_official_namespace_reports_an_incomplete_distribution() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("aster-missing-stdlib-{nonce}.aster"));
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
