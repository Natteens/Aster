mod general;

use std::collections::{HashMap, HashSet};

use aster_diagnostics::{Diagnostic, Span};
use aster_hir::StringOperation;
use aster_syntax::{
    AssignmentOperator, Expression, ExpressionKind, Item, Literal, MemberInitializer, Module,
    SwitchCase, SwitchExpressionCase, TypeRef,
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

/// Identity of a callable declaration within one compilation: owner, name,
/// parameter type spelling, and accessor kind. `declaration_start` is kept
/// only to order candidates deterministically for diagnostics; it is not part
/// of the identity, so the key does not change when the source moves.
#[derive(Clone, Debug)]
pub(crate) struct CallableKey {
    pub owner: Option<String>,
    pub declaration_start: usize,
    pub name: String,
    pub signature: String,
    pub accessor: Option<AccessorKind>,
}

impl PartialEq for CallableKey {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.name == other.name
            && self.signature == other.signature
            && self.accessor == other.accessor
    }
}

impl Eq for CallableKey {}

impl std::hash::Hash for CallableKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.owner.hash(state);
        self.name.hash(state);
        self.signature.hash(state);
        self.accessor.hash(state);
    }
}

/// Parameter type spelling that distinguishes overloads, e.g. `int,string`.
pub(crate) fn parameter_signature(parameters: &[aster_syntax::Parameter]) -> String {
    parameters
        .iter()
        .map(|parameter| parameter.type_ref.name.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn callable_key(
    name: &str,
    declaration_start: usize,
    parameters: &[aster_syntax::Parameter],
    accessor: Option<AccessorKind>,
    owner: Option<&str>,
) -> CallableKey {
    CallableKey {
        owner: owner.map(str::to_owned),
        declaration_start,
        name: name.to_owned(),
        signature: parameter_signature(parameters),
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
    Clear,
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

/// The `Option<T>` an `value as T` expression produces: the concrete
/// specialization and the positions of its `Some` and `None` cases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedTypeAs {
    pub option_type: String,
    pub some_index: usize,
    pub none_index: usize,
}

/// The constructor of the base class a derived constructor runs: an explicit
/// `: base(arguments)` or the implicit call of a base constructor whose
/// parameters all have defaults. `arguments` binds the source-ordered
/// arguments (none when implicit) and the evaluated defaults.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedBaseConstructor {
    pub callable: CallableKey,
    pub arguments: ResolvedArguments,
}

/// How one instance method or property accessor uses `this`; input to the
/// this-confinement fixpoint (`general::this_confinement`).
#[derive(Clone, Debug, Default)]
pub(crate) struct ThisSummary {
    pub escape: Option<Span>,
    /// The first virtual or abstract member called on `this` (dynamic
    /// dispatch): a body that does so is not this-confined.
    pub virtual_call: Option<Span>,
    pub reads: std::collections::BTreeSet<String>,
    pub calls: Vec<CallableKey>,
}

/// One call on `this` inside a constructor, checked once every method's
/// summary is known.
#[derive(Clone, Debug)]
pub(crate) struct ConstructorThisCall {
    pub class: String,
    pub callable: CallableKey,
    pub span: Span,
    pub unassigned: Vec<String>,
    /// The call is dispatched dynamically (a virtual, abstract or overriding
    /// member reached without `base`).
    pub virtual_dispatch: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Model {
    /// Each `override` callable mapped to the member it directly overrides.
    pub overrides: HashMap<CallableKey, CallableKey>,
    /// Base constructor resolved for each constructor of a derived class,
    /// keyed by the constructor's model context.
    pub base_constructors: HashMap<String, ResolvedBaseConstructor>,
    pub this_summaries: HashMap<CallableKey, ThisSummary>,
    pub constructor_this_calls: Vec<ConstructorThisCall>,
    pub calls: HashMap<ModelNodeKey, ResolvedCall>,
    pub call_arguments: HashMap<ModelNodeKey, ResolvedArguments>,
    /// Call sites resolved to a concrete host-provided foreign declaration.
    /// HIR consumes this semantic decision; later stages never inspect names.
    pub foreign_calls: HashSet<ModelNodeKey>,
    pub constructors: HashMap<ModelNodeKey, CallableKey>,
    pub property_reads: HashMap<ModelNodeKey, CallableKey>,
    /// Reads and writes of a statically bound auto-property that HIR lowers
    /// directly to its synthesized backing field (LANG-005), keyed by the
    /// `Name` or `Member` expression, mapped to the backing field name. There
    /// is no inliner: the accessor call is skipped at lowering time, so the
    /// backend sees a plain field access.
    pub auto_property_fields: HashMap<ModelNodeKey, String>,
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
    /// Numeric type to which both operands of a binary expression are
    /// converted before the operation (the promoted common type, or the
    /// promoted left operand of a shift). Absent when no numeric conversion
    /// applies. HIR lowering consumes this decision instead of repeating the
    /// promotion rules.
    pub binary_operand_types: HashMap<ModelNodeKey, String>,
    /// Node ordinals per model context; see [`Model::node_key`].
    pub node_indexes: HashMap<String, HashMap<Span, u32>>,
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
    /// `value as T` sites and the `Option<T>` specialization each produces.
    pub type_as: HashMap<ModelNodeKey, ResolvedTypeAs>,
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

/// A syntax node within one model context. `Ordinal` is the node's pre-order
/// position among the indexed nodes of its context, so it does not change
/// when whitespace or other declarations move. `Position` is the fallback for
/// a node the index does not cover; it keeps lookups consistent but is not
/// stable across edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum NodeRef {
    Ordinal(u32),
    Position(Span),
}

/// Identity is `(context, node)`. `span` is the node's current source position,
/// kept for diagnostics and deterministic report order only.
#[derive(Clone, Debug)]
pub(crate) struct ModelNodeKey {
    pub context: String,
    pub node: NodeRef,
    pub span: Span,
}

impl PartialEq for ModelNodeKey {
    fn eq(&self, other: &Self) -> bool {
        self.context == other.context && self.node == other.node
    }
}

impl Eq for ModelNodeKey {}

impl std::hash::Hash for ModelNodeKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.context.hash(state);
        self.node.hash(state);
    }
}

/// Pre-order ordinals of the expressions, statements, blocks, switch cases,
/// and variable declarations of one context. A span shared by several nodes
/// takes its first ordinal, so two nodes share an ordinal exactly when they
/// share a span: the key equivalence of positional keys is preserved.
#[derive(Default)]
struct NodeIndexer {
    ordinals: HashMap<Span, u32>,
}

impl NodeIndexer {
    fn record(&mut self, span: Span) {
        let next = u32::try_from(self.ordinals.len()).unwrap_or(u32::MAX);
        self.ordinals.entry(span).or_insert(next);
    }
}

impl aster_syntax::visit::AstVisitorMut for NodeIndexer {
    fn visit_block_mut(&mut self, block: &mut aster_syntax::Block) {
        self.record(block.span);
        aster_syntax::visit::walk_block_mut(self, block);
    }

    fn visit_statement_mut(&mut self, statement: &mut aster_syntax::Statement) {
        self.record(statement.span());
        aster_syntax::visit::walk_statement_mut(self, statement);
    }

    fn visit_variable_declaration_mut(
        &mut self,
        declaration: &mut aster_syntax::VariableDeclaration,
    ) {
        self.record(declaration.span);
        aster_syntax::visit::walk_variable_declaration_mut(self, declaration);
    }

    fn visit_switch_case_mut(&mut self, case: &mut aster_syntax::SwitchCase) {
        self.record(case.span);
        aster_syntax::visit::walk_switch_case_mut(self, case);
    }

    fn visit_switch_expression_case_mut(&mut self, case: &mut aster_syntax::SwitchExpressionCase) {
        self.record(case.span);
        aster_syntax::visit::walk_switch_expression_case_mut(self, case);
    }

    fn visit_expression_mut(&mut self, expression: &mut aster_syntax::Expression) {
        self.record(expression.span);
        aster_syntax::visit::walk_expression_mut(self, expression);
    }
}

impl Model {
    /// Index the nodes of a function body (and parameter defaults) under
    /// `context`. Semantic analysis registers every context it analyzes.
    pub(crate) fn register_function(
        &mut self,
        context: &str,
        function: &aster_syntax::FunctionDeclaration,
    ) {
        let mut indexer = NodeIndexer::default();
        let mut copy = function.clone();
        aster_syntax::visit::AstVisitorMut::visit_function_declaration_mut(&mut indexer, &mut copy);
        self.node_indexes
            .insert(context.to_owned(), indexer.ordinals);
    }

    /// Index the nodes of one expression (a field initializer) under `context`.
    pub(crate) fn register_expression(
        &mut self,
        context: &str,
        expression: &aster_syntax::Expression,
    ) {
        let mut indexer = NodeIndexer::default();
        let mut copy = expression.clone();
        aster_syntax::visit::AstVisitorMut::visit_expression_mut(&mut indexer, &mut copy);
        self.node_indexes
            .insert(context.to_owned(), indexer.ordinals);
    }

    /// The single constructor of model keys, used by semantic analysis and
    /// HIR lowering alike.
    pub(crate) fn node_key(&self, context: &str, span: Span) -> ModelNodeKey {
        let node = self
            .node_indexes
            .get(context)
            .and_then(|ordinals| ordinals.get(&span))
            .map_or(NodeRef::Position(span), |ordinal| {
                NodeRef::Ordinal(*ordinal)
            });
        ModelNodeKey {
            context: context.to_owned(),
            node,
            span,
        }
    }
}

pub(crate) fn function_context(
    function: &aster_syntax::FunctionDeclaration,
    owner: Option<&aster_syntax::TypeDeclaration>,
) -> String {
    format!(
        "{}::{}({})",
        owner.map_or("#module", |owner| owner.name.as_str()),
        function.name,
        parameter_signature(&function.parameters)
    )
}

/// Model context of a property accessor body. Equals [`function_context`] of
/// the `get_`/`set_` function that `validate_property` synthesizes for it, so
/// HIR lowering finds every decision semantic analysis recorded in the body.
pub(crate) fn accessor_context(
    owner: &str,
    property: &str,
    kind: AccessorKind,
    property_type: &str,
) -> String {
    match kind {
        AccessorKind::Get => format!("{owner}::get_{property}()"),
        AccessorKind::Set => format!("{owner}::set_{property}({property_type})"),
    }
}

pub(crate) fn field_context(owner: &str, field: &str) -> String {
    format!("{owner}::#field:{field}")
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

/// The hidden binding naming the object under construction in an object
/// initializer. Not a valid identifier, so source can never name it.
pub(crate) const INITIALIZER_RECEIVER: &str = "$init";

/// The synthesized `$init.Member = value` assignment for one member of an
/// object initializer. Semantic analysis and HIR lowering both build it from
/// the same spans so their model entries agree.
pub(crate) fn initializer_assignment(member: &MemberInitializer) -> Expression {
    let receiver_span = Span::new(member.name_span.start, member.name_span.start);
    Expression {
        kind: ExpressionKind::Assignment {
            target: Box::new(Expression {
                kind: ExpressionKind::Member {
                    object: Box::new(Expression {
                        kind: ExpressionKind::Name(INITIALIZER_RECEIVER.to_owned()),
                        span: receiver_span,
                    }),
                    name: member.name.clone(),
                },
                span: member.name_span,
            }),
            operator: AssignmentOperator::Assign,
            value: Box::new(member.value.clone()),
        },
        span: member.span,
    }
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

#[cfg(test)]
impl Model {
    /// Every key of every decision table, with the values that are plain
    /// type or callable names. Positions are excluded by construction.
    pub(crate) fn key_fingerprint(&self) -> Vec<String> {
        fn key(table: &str, key: &ModelNodeKey) -> String {
            format!("{table} {} {:?}", key.context, key.node)
        }
        fn callable(key: &CallableKey) -> String {
            format!(
                "{:?}::{:?}({}) {:?}",
                key.owner, key.name, key.signature, key.accessor
            )
        }
        let mut out = Vec::new();
        macro_rules! keys {
            ($($field:ident),*) => {
                $(for k in self.$field.keys() { out.push(key(stringify!($field), k)); })*
            };
        }
        macro_rules! members {
            ($($field:ident),*) => {
                $(for k in &self.$field { out.push(key(stringify!($field), k)); })*
            };
        }
        keys!(
            calls,
            call_arguments,
            property_assignments,
            auto_property_fields,
            enum_values,
            task_runs,
            parallel_for,
            parallel_for_each,
            parallel_reduce,
            switch_cases,
            propagations,
            type_as,
            string_operations,
            dictionary_operations,
            string_builder_operations
        );
        members!(
            foreign_calls,
            list_indexes,
            zero_length_new_arrays,
            string_builder_constructions,
            format_primitives
        );
        for (k, v) in &self.calls {
            out.push(format!(
                "{} -> {}",
                key("call target", k),
                callable(&v.callable)
            ));
        }
        for (k, v) in &self.constructors {
            out.push(format!("{} -> {}", key("constructors", k), callable(v)));
        }
        for (k, v) in &self.property_reads {
            out.push(format!("{} -> {}", key("property_reads", k), callable(v)));
        }
        for (table, map) in [
            ("switch_expression_types", &self.switch_expression_types),
            ("binary_operand_types", &self.binary_operand_types),
            (
                "contextual_empty_array_elements",
                &self.contextual_empty_array_elements,
            ),
            ("contextual_new_types", &self.contextual_new_types),
        ] {
            for (k, v) in map {
                out.push(format!("{} -> {v}", key(table, k)));
            }
        }
        out.sort();
        out
    }

    pub(crate) fn position_fallbacks(&self) -> usize {
        self.key_fingerprint()
            .iter()
            .filter(|line| line.contains("Position("))
            .count()
    }
}

#[cfg(test)]
mod structural_identity_tests {
    use super::Model;

    fn model_of(source: &str) -> Model {
        let tokens = aster_syntax::lex(source).expect("lexes");
        let mut module = aster_syntax::parse(tokens).expect("parses");
        assert_eq!(super::validate_deferred_language_surfaces(&mut module), []);
        crate::synthesize_default_constructors(&mut module);
        assert_eq!(crate::generics::monomorphize(&mut module), []);
        let (diagnostics, model) = super::validate(&module);
        assert!(
            diagnostics
                .iter()
                .all(|d| d.severity != aster_diagnostics::Severity::Error),
            "{diagnostics:?}"
        );
        model
    }

    const ORIGINAL: &str = r#"
public enum Shape { Circle(int radius), Square(int side) }

public class Box<T>
{
    private T item;
    public Box(T item) { this.item = item; }
    public T Get() { return item; }
}

public class Meter
{
    private int total = 1 + 2;
    private byte small;
    public int Total
    {
        get { return total + small; }
        set { total = value * 2; }
    }
    public int Add(int value) { total += value; return total; }
    public long Add(long value) { return (long)total + value; }
}

public int Square(int value) { return value * value; }

public int Area(Shape shape)
{
    switch (shape)
    {
        case Circle(radius):
            return radius * radius * 3;
        case Square(side):
            return side * side;
    }
}

public int Run()
{
    Box<int> ints = new Box<int>(4);
    Box<string> words = new Box<string>("ab");
    Meter meter = new Meter();
    meter.Total = 3;
    meter.Add(2);
    long wide = meter.Add(5L);
    List<int> values = new List<int>();
    values.Add(ints.Get());
    values[0] += 1;
    Task<int> task = Task.Run(Square, 6);
    string text = $"{values[0]}:{words.Get().Length}";
    return Area(Shape.Circle(2)) + meter.Total + task.Wait() + text.Length + (int)wide;
}
"#;

    /// Same declarations: different whitespace, comments, a new unrelated
    /// function first, and `Square`/`Area` swapped.
    const MOVED: &str = r#"
// a leading comment shifts every position
public int Unrelated() { return 0; }

public enum Shape { Circle(int radius),    Square(int side) }

public class Box<T>
{

    private T item;
    public Box(T item)
    {
        this.item = item;
    }
    public T Get() {   return item;   }
}

public class Meter
{
    private int total = 1   +   2;
    private byte small;
    public int Total
    {
        get
        {
            return total + small;
        }
        set { total = value * 2; }
    }
    public int Add(int value) { total += value;
        return total; }
    public long Add(long value) { return (long)total + value; }
}

public int Area(Shape shape)
{
    switch (shape)
    {
        case Circle(radius):
            return radius * radius * 3;
        case Square(side):
            return side * side;
    }
}

public int Square(int value) {
    return value * value;
}

public int Run()
{
    Box<int> ints = new Box<int>(4);
    Box<string> words = new Box<string>("ab");
    Meter meter = new Meter();
    meter.Total = 3;
    meter.Add(2);
    long wide = meter.Add(5L);
    List<int> values = new List<int>();
    values.Add(ints.Get());
    values[0] += 1;
    Task<int> task = Task.Run(Square, 6);
    string text = $"{values[0]}:{words.Get().Length}";
    return Area(Shape.Circle(2))  +  meter.Total + task.Wait() + text.Length + (int)wide;
}
"#;

    #[test]
    fn every_key_survives_whitespace_comment_and_position_edits() {
        let original = model_of(ORIGINAL);
        let moved = model_of(MOVED);
        assert_eq!(
            original.position_fallbacks(),
            0,
            "{:#?}",
            original.key_fingerprint()
        );
        let unrelated: Vec<_> = moved
            .key_fingerprint()
            .into_iter()
            .filter(|line| !line.contains("::Unrelated("))
            .collect();
        assert_eq!(original.key_fingerprint(), unrelated);
    }

    #[test]
    fn the_fingerprint_covers_generics_overloads_properties_and_calls() {
        let fingerprint = model_of(ORIGINAL).key_fingerprint().join("\n");
        for expected in [
            "Box<int>",
            "Box<string>",
            "\"Add\"(int)",
            "\"Add\"(long)",
            "Some(Get)",
            "property_assignments",
            "::get_Total()",
            "::set_Total(int)",
            "#field:total",
            "task_runs",
            "switch_cases",
            "list_indexes",
        ] {
            assert!(
                fingerprint.contains(expected),
                "missing `{expected}` in\n{fingerprint}"
            );
        }
    }

    #[test]
    fn a_structural_body_edit_changes_only_the_edited_context() {
        let edited = ORIGINAL.replace(
            "public int Square(int value) { return value * value; }",
            "public int Square(int value) { int doubled = value + value; return value * value; }",
        );
        let original = model_of(ORIGINAL).key_fingerprint();
        let changed = model_of(&edited).key_fingerprint();
        assert_ne!(original, changed);
        let outside: Vec<_> = original
            .iter()
            .filter(|line| !line.contains("#module::Square("))
            .collect();
        let outside_changed: Vec<_> = changed
            .iter()
            .filter(|line| !line.contains("#module::Square("))
            .collect();
        assert_eq!(
            outside, outside_changed,
            "keys outside the edited body must not move"
        );
    }
}
