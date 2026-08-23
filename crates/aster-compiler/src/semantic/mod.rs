mod general;

use std::collections::{HashMap, HashSet};

use aster_diagnostics::{Diagnostic, Span};
use aster_hir::StringOperation;
use aster_syntax::{
    Expression, ExpressionKind, Item, Literal, Module, SwitchCase, SwitchExpressionCase, TypeRef,
    visit::{
        AstVisitorMut, walk_expression_mut, walk_switch_case_mut, walk_switch_expression_case_mut,
    },
};

use crate::constexpr::ConstValue;
use crate::type_names::TypeName;

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedDefaultArgument {
    pub parameter: usize,
    pub value: ConstValue,
}

/// Candidate-specific binding of source-ordered arguments to declaration
/// parameters. Defaults are already evaluated constants, so HIR never owns
/// source-level optional-parameter semantics.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct ResolvedArguments {
    pub source_to_parameter: Vec<usize>,
    pub defaults: Vec<ResolvedDefaultArgument>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedPropertyAssignment {
    pub getter: Option<CallableKey>,
    pub setter: CallableKey,
}

/// A resolved `aster.core.Task.Run(function, arguments...)`: the concrete
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

/// A resolved `Parallel.Reduce(values, identity, Accumulate, Combine)`:
/// `Accumulate`'s and `Combine`'s concrete, zero-capture targets, resolved
/// once here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedParallelReduce {
    pub accumulate: CallableKey,
    pub combine: CallableKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedEnumCase {
    pub enum_name: String,
    pub case_index: usize,
    pub argument_order: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedDictionaryOperation {
    Basic,
    TryGet {
        option_type: String,
        some_index: usize,
        none_index: usize,
    },
    Entries {
        entry_type: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedStringBuilderOperation {
    Append,
    ToString,
}

/// A resolved postfix `?` operator. Records the concrete official-`Result`
/// or official-`Option` enum and case positions so HIR lowering can attach
/// symbols and tags without re-resolving any type or inspecting names. The
/// two variants are kept structurally distinct (rather than one shape with
/// an optional error) because `Option`'s `None` carries no payload at all,
/// unlike `Result`'s `Error`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedPropagation {
    Result {
        result_type: String,
        ok_index: usize,
        error_index: usize,
        function_result_type: String,
        function_error_index: usize,
    },
    Option {
        option_type: String,
        some_index: usize,
        none_index: usize,
        function_option_type: String,
        function_none_index: usize,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Model {
    pub calls: HashMap<ModelNodeKey, ResolvedCall>,
    pub call_arguments: HashMap<ModelNodeKey, ResolvedArguments>,
    /// Call sites resolved to a concrete host-provided foreign declaration.
    /// HIR consumes this semantic decision; later stages never inspect names.
    pub foreign_calls: HashSet<ModelNodeKey>,
    pub constructors: HashMap<ModelNodeKey, CallableKey>,
    pub property_reads: HashMap<ModelNodeKey, CallableKey>,
    pub property_assignments: HashMap<ModelNodeKey, ResolvedPropertyAssignment>,
    pub enum_values: HashMap<ModelNodeKey, ResolvedEnumCase>,
    pub task_runs: HashMap<ModelNodeKey, ResolvedTaskRun>,
    pub parallel_for: HashMap<ModelNodeKey, ResolvedParallelFor>,
    pub parallel_for_each: HashMap<ModelNodeKey, ResolvedParallelForEach>,
    pub parallel_reduce: HashMap<ModelNodeKey, ResolvedParallelReduce>,
    pub switch_cases: HashMap<ModelNodeKey, ResolvedEnumCase>,
    /// Common result type selected by semantic analysis for a restricted enum
    /// switch expression. HIR lowering materializes this decision instead of
    /// independently repeating promotion or compatibility rules.
    pub switch_expression_types: HashMap<ModelNodeKey, String>,
    /// Exact element type supplied by an explicit variable declaration for
    /// an otherwise untyped empty array literal. This is deliberately narrow:
    /// ordinary expression inference remains one-way.
    pub contextual_empty_array_elements: HashMap<ModelNodeKey, String>,
    /// Exact constructible type supplied to a target-typed `new(...)` site.
    /// Source spelling is intentionally absent; HIR consumes this semantic
    /// decision and receives an ordinary concrete construction.
    pub contextual_new_types: HashMap<ModelNodeKey, String>,
    /// Index expressions semantically resolved against `List<T>` rather than
    /// an array. HIR lowers these through the existing List Get/Set runtime
    /// boundary; length never changes the type-based decision.
    pub list_indexes: HashSet<ModelNodeKey>,
    /// `new T[length]` sites whose length is proven constant zero by semantic
    /// constant evaluation. Lowering uses this proof to state that no default
    /// element slots exist; later layers never repeat source-level reasoning.
    pub zero_length_new_arrays: HashSet<ModelNodeKey>,
    pub propagations: HashMap<ModelNodeKey, ResolvedPropagation>,
    pub string_operations: HashMap<ModelNodeKey, StringOperation>,
    pub dictionary_operations: HashMap<ModelNodeKey, ResolvedDictionaryOperation>,
    /// Intrinsic operations on the official `aster.core.StringBuilder`,
    /// selected after ordinary nominal method resolution. HIR lowering
    /// consumes this semantic decision instead of recognizing method names.
    pub string_builder_operations: HashMap<ModelNodeKey, ResolvedStringBuilderOperation>,
    /// Successful construction sites for the official `StringBuilder` class.
    pub string_builder_constructions: HashSet<ModelNodeKey>,
    /// Validated `value.ToString()` call sites on one of the eight
    /// fundamental primitives. Membership alone is enough for HIR lowering:
    /// the receiver's own lowered type supplies the exact primitive.
    pub format_primitives: HashSet<ModelNodeKey>,
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

pub(crate) fn validate_deferred_language_surfaces(module: &mut Module) -> Vec<Diagnostic> {
    let mut validator = DeferredLanguageSurfaceValidator {
        diagnostics: Vec::new(),
    };
    validator.visit_module_mut(module);
    validator.diagnostics
}

struct DeferredLanguageSurfaceValidator {
    diagnostics: Vec<Diagnostic>,
}

impl AstVisitorMut for DeferredLanguageSurfaceValidator {
    fn visit_expression_mut(&mut self, expression: &mut Expression) {
        match &expression.kind {
            ExpressionKind::Literal(Literal::Decimal(_)) => {
                self.diagnostics.push(decimal_deferred(expression.span));
            }
            ExpressionKind::Name(name) => self.validate_type_name(name, expression.span),
            ExpressionKind::StructLiteral { type_name, .. }
            | ExpressionKind::NewObject {
                type_name: Some(type_name),
                ..
            } => self.validate_type_name(type_name, expression.span),
            _ => {}
        }
        walk_expression_mut(self, expression);
    }

    fn visit_switch_case_mut(&mut self, case: &mut SwitchCase) {
        if let Some(owner) = &case.enum_name {
            self.validate_type_name(owner, case.span);
        }
        walk_switch_case_mut(self, case);
    }

    fn visit_switch_expression_case_mut(&mut self, case: &mut SwitchExpressionCase) {
        if let Some(owner) = &case.enum_name {
            self.validate_type_name(owner, case.span);
        }
        walk_switch_expression_case_mut(self, case);
    }

    fn visit_type_ref_mut(&mut self, type_ref: &mut TypeRef) {
        self.validate_type_name(&type_ref.name, type_ref.span);
    }
}

impl DeferredLanguageSurfaceValidator {
    fn validate_type_name(&mut self, name: &str, span: Span) {
        if TypeName::parse(name).is_some_and(|type_name| contains_decimal(&type_name)) {
            self.diagnostics.push(decimal_deferred(span));
        }
    }
}

fn contains_decimal(type_name: &TypeName) -> bool {
    type_name.base == "decimal" || type_name.arguments.iter().any(contains_decimal)
}

fn decimal_deferred(span: Span) -> Diagnostic {
    Diagnostic::error(
        "`decimal` is reserved but not supported until its exact precision, scale, rounding, overflow, formatting, layout, and ABI contract is specified",
        span,
    )
    .with_help("use an integer in explicit minor units, or `float`/`double` for approximate values")
}

/// `Task`, `Parallel`, `List`, and `Dictionary` are reserved, intrinsic names
/// (`aster.core.Task<T>`, see `hir::Type::Task`, the
/// `Parallel.For`/`Parallel.ForEach` surface, and `aster.core.List<T>`, see
/// `hir::Type::List`): no class, struct, interface, or enum declaration
/// (generic or not) may use any of them. This is the single place that
/// reservation is enforced, so every later stage can recognize these names
/// structurally without checking whether a user redefined them.
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
        if name == "List" {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`List` is reserved for the built-in `List<T>` type and cannot be declared as a {kind}"
                    ),
                    span,
                )
                .with_help("rename this type; `List<T>` is a built-in name, not something a program can declare"),
            );
        }
        if name == "Dictionary" {
            diagnostics.push(
                Diagnostic::error(
                    format!("`Dictionary` is reserved for the built-in `Dictionary<K, V>` type and cannot be declared as a {kind}"),
                    span,
                )
                .with_help("rename this type; `Dictionary<K, V>` is a built-in name, not something a program can declare"),
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
