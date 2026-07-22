mod general;

use std::collections::HashMap;

use aster_diagnostics::{Diagnostic, Span};
use aster_syntax::{Item, Module};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AccessorKind {
    Get,
    Set,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CallableKey {
    pub owner: Option<String>,
    pub declaration_start: usize,
    pub name: String,
    pub accessor: Option<AccessorKind>,
}

pub(crate) fn callable_key(
    name: &str,
    declaration_start: usize,
    accessor: Option<AccessorKind>,
    owner: Option<&str>,
) -> CallableKey {
    CallableKey {
        owner: owner.map(str::to_owned),
        declaration_start,
        name: name.to_owned(),
        accessor,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dispatch {
    Direct,
    Instance,
    Interface,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedCall {
    pub callable: CallableKey,
    pub dispatch: Dispatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedPropertyAssignment {
    pub getter: Option<CallableKey>,
    pub setter: CallableKey,
}

/// A resolved `aster.core.Task.Run(function)`: the concrete zero-parameter
/// free function or static method `function` names, resolved once here so
/// HIR lowering never re-resolves the argument by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedTaskRun {
    pub function: CallableKey,
}

/// A resolved `Parallel.For(start, end, Body)`: `Body`'s concrete zero-capture
/// `void(int)` target, resolved once here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedParallelFor {
    pub body: CallableKey,
}

/// A resolved `Parallel.ForEach(values, Body)`: `Body`'s concrete
/// zero-capture `void(T)` target, resolved once here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedParallelForEach {
    pub body: CallableKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedEnumCase {
    pub enum_name: String,
    pub case_index: usize,
}

/// A resolved postfix `?` operator. Records the concrete official-`Result`
/// enums and case positions so HIR lowering can attach symbols and tags without
/// re-resolving any type or inspecting names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedPropagation {
    pub result_type: String,
    pub ok_index: usize,
    pub error_index: usize,
    pub function_result_type: String,
    pub function_error_index: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Model {
    pub calls: HashMap<ModelNodeKey, ResolvedCall>,
    pub constructors: HashMap<ModelNodeKey, CallableKey>,
    pub property_reads: HashMap<ModelNodeKey, CallableKey>,
    pub property_assignments: HashMap<ModelNodeKey, ResolvedPropertyAssignment>,
    pub enum_values: HashMap<ModelNodeKey, ResolvedEnumCase>,
    pub task_runs: HashMap<ModelNodeKey, ResolvedTaskRun>,
    pub parallel_for: HashMap<ModelNodeKey, ResolvedParallelFor>,
    pub parallel_for_each: HashMap<ModelNodeKey, ResolvedParallelForEach>,
    pub switch_cases: HashMap<ModelNodeKey, ResolvedEnumCase>,
    pub propagations: HashMap<ModelNodeKey, ResolvedPropagation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ModelNodeKey {
    pub context: String,
    pub span: Span,
}

pub(crate) fn function_context(
    function: &aster_syntax::FunctionDeclaration,
    owner: Option<&aster_syntax::TypeDeclaration>,
) -> String {
    format!(
        "{}::{}@{}",
        owner.map_or("#module", |owner| owner.name.as_str()),
        function.name,
        function.span.start
    )
}

pub(crate) fn field_context(owner: &str, field: &str, start: usize) -> String {
    format!("{owner}::#field:{field}@{start}")
}

pub(super) fn validate(module: &Module) -> (Vec<Diagnostic>, Model) {
    let mut diagnostics = Vec::new();
    validate_declaration_names(module, &mut diagnostics);
    validate_no_reserved_type_names(module, &mut diagnostics);
    let mut model = Model::default();
    general::validate(module, &mut diagnostics, &mut model);
    (diagnostics, model)
}

/// `Task` and `Parallel` are reserved, intrinsic names (`aster.core.Task<T>`,
/// see `hir::Type::Task`, and the `Parallel.For`/`Parallel.ForEach` surface):
/// no class, struct, interface, or enum declaration (generic or not) may use
/// either. This is the single place that reservation is enforced, so every
/// later stage can recognize these names structurally without checking
/// whether a user redefined them.
fn validate_no_reserved_type_names(module: &Module, diagnostics: &mut Vec<Diagnostic>) {
    for item in &module.items {
        let (kind, name, span) = match item {
            Item::Class(item) => ("class", &item.name, item.span),
            Item::Struct(item) => ("struct", &item.name, item.span),
            Item::Interface(item) => ("interface", &item.name, item.span),
            Item::Enum(item) => ("enum", &item.name, item.span),
            Item::Function(_) | Item::Variable(_) => continue,
        };
        if name == "Task" || name == "Parallel" {
            diagnostics.push(
                Diagnostic::error(
                    format!("`{name}` is reserved for the intrinsic concurrency system and cannot be declared as a {kind}"),
                    span,
                )
                .with_help("rename this type; it is a built-in name, not something a program can declare"),
            );
        }
    }
}

fn validate_declaration_names(module: &Module, diagnostics: &mut Vec<Diagnostic>) {
    let mut declarations = HashMap::new();
    for item in &module.items {
        let (name, span) = item_name(item);
        let overloadable = matches!(item, Item::Function(_));
        if declarations
            .insert(name, (span, overloadable))
            .is_some_and(|(_, previous)| !overloadable || !previous)
        {
            diagnostics.push(
                Diagnostic::error(format!("duplicate declaration `{name}`"), span)
                    .with_help("rename or remove one of the declarations"),
            );
        }
    }
}

fn item_name(item: &Item) -> (&str, Span) {
    match item {
        Item::Class(item) | Item::Struct(item) | Item::Interface(item) => (&item.name, item.span),
        Item::Enum(item) => (&item.name, item.span),
        Item::Function(item) => (&item.name, item.span),
        Item::Variable(item) => (&item.name, item.span),
    }
}
