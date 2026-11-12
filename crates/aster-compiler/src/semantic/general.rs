use std::collections::{HashMap, HashSet};

use aster_diagnostics::{Diagnostic, Span};

use crate::constexpr::{ConstError, ConstValue, evaluate, integer_value};
use crate::primitives::{
    self, IntegerFit, Primitive, UnsignedFit, classify_integer, classify_unsigned, fits_long,
    fits_ulong,
};
use aster_syntax::{
    Argument, AssignmentOperator, BinaryOperator, Block, Expression, ExpressionKind, Field,
    FunctionDeclaration, IncrementOperator, InterpolatedPart, Item, Literal, Member,
    MemberDispatch, MemberInitializer, Module, Property, Statement, TypeDeclaration, TypeRef,
    UnaryOperator, VariableDeclaration, VariableKind, Visibility,
};

use super::{
    AccessorKind, CallableKey, ConstructorThisCall, Dispatch, Model, ResolvedArguments,
    ResolvedBaseConstructor, ResolvedCall, ResolvedDefaultArgument, ResolvedDictionaryOperation,
    ResolvedEnumCase, ResolvedParallelFor, ResolvedParallelForEach, ResolvedParallelReduce,
    ResolvedPropagation, ResolvedPropertyAssignment, ResolvedStringBuilderOperation,
    ResolvedTaskRun, ResolvedTypeAs, ThisSummary, callable_key,
};
use crate::type_names::TypeName;

mod calls;
mod declarations;
mod expressions;
mod inheritance;
mod nested_concurrency;
mod statements;
mod this_confinement;
mod types;

// These modules extend one analyzer and share this private semantic domain. Keeping the state here
// prevents scopes, symbol tables, model writes, and diagnostics from being duplicated per phase.
use types::{
    Type, compatible, constant_integer, promoted_numeric, resolve_type, resolve_type_readonly,
    zero_initializable,
};

pub(super) fn validate(module: &Module, diagnostics: &mut Vec<Diagnostic>, model: &mut Model) {
    let mut context = Context::default();
    // This order is semantic: names support forward type references, declarations populate the
    // complete symbol tables, and only then may relationship checks and ordered body analysis run.
    declarations::collect_type_names(module, &mut context);
    declarations::collect_declarations(module, &mut context, diagnostics);
    declarations::discover_official_types(&mut context);
    inheritance::resolve(module, &mut context, diagnostics);
    model.overrides.clone_from(&context.overrides);
    declarations::validate_interface_implementations(module, &context, diagnostics);
    declarations::validate_struct_cycles(module, &context, diagnostics);
    declarations::validate_module_variables(module, &mut context, diagnostics, model);
    declarations::validate_bodies(module, &context, diagnostics, model);
    // Needs the summary of every method body, so it runs after all of them.
    this_confinement::validate(&context, model, diagnostics);
    // Runs after every call/Task.Run/Parallel resolution has been recorded in
    // `model`, so the call graph it walks is complete.
    nested_concurrency::validate(module, &context, model, diagnostics);
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    parameters: Vec<Type>,
    result: Type,
}

#[derive(Clone, Debug)]
struct Callable {
    signature: Signature,
    parameter_names: Vec<String>,
    defaults: Vec<Option<Expression>>,
    visibility: Visibility,
    is_static: bool,
    is_foreign: bool,
    /// `virtual`, `override` or `abstract` (methods and property accessors).
    dispatch: MemberDispatch,
    key: CallableKey,
}

#[derive(Clone, Debug)]
struct PropertyInfo {
    type_: Type,
    getter: Option<Callable>,
    setter: Option<Callable>,
    /// The class or struct that declares the property.
    declared_in: String,
    /// The synthesized private backing field when this is an auto-property
    /// (LANG-005); its accessors are trivial reads and writes of that field.
    auto_backing: Option<String>,
    /// The setter is an `init` accessor: callable only while the object is
    /// under construction.
    init_only: bool,
}

impl PropertyInfo {
    /// Whether any accessor dispatches dynamically (`virtual`, `override`,
    /// `abstract`); such an access must keep calling the accessor.
    fn is_virtual(&self) -> bool {
        self.getter
            .iter()
            .chain(self.setter.iter())
            .any(|accessor| accessor.dispatch != MemberDispatch::None)
    }

    /// The backing field a use of this property may be lowered to directly:
    /// an auto-property whose accessors are statically bound.
    fn direct_backing(&self) -> Option<&str> {
        self.auto_backing.as_deref().filter(|_| !self.is_virtual())
    }
}

/// How a diagnostic names a field: a synthesized auto-property backing field
/// is reported as its property, since the field name is not writable source.
fn field_display(field: &str) -> String {
    field
        .strip_prefix('<')
        .and_then(|name| name.strip_suffix(">k__BackingField"))
        .map_or_else(
            || format!("field `{field}`"),
            |property| format!("auto-property `{property}`"),
        )
}

#[derive(Clone, Debug, Default)]
struct TypeInfo {
    is_static: bool,
    fields: HashMap<String, Type>,
    field_order: Vec<String>,
    field_visibility: HashMap<String, Visibility>,
    methods: HashMap<String, Vec<Callable>>,
    properties: HashMap<String, PropertyInfo>,
    /// Every declared constructor (B2 overloads), in source order. Empty for
    /// a class without one.
    constructors: Vec<Callable>,
    /// Interfaces the class lists itself followed by every interface its base
    /// chain implements (a derived class may not list one again).
    implemented_interfaces: Vec<String>,
    /// The base class, when the first name after `:` is a class.
    base: Option<String>,
    is_abstract: bool,
    is_sealed: bool,
    /// Accessible inherited fields mapped to their declaring class. Own fields
    /// are absent; the inherited members are merged into `fields`, `methods`
    /// and `properties` so lookups need no chain walk.
    field_owner: HashMap<String, String>,
    /// Base classes, nearest first. Empty for a class without a base.
    ancestors: Vec<String>,
    /// Private members of the base chain (name -> declaring class). They are
    /// not members of this class; the table only sharpens diagnostics.
    hidden_private: HashMap<String, String>,
    kind: Option<TypeKind>,
    enum_cases: Vec<EnumCaseInfo>,
}

#[derive(Clone, Debug)]
struct EnumCaseInfo {
    name: String,
    fields: Vec<(String, Type)>,
}

/// Resolved `Ok`/`Error` positions and payload types of an official `Result`.
struct ResultCases {
    ok_index: usize,
    error_index: usize,
    success: Type,
    error: Type,
}

/// Resolved `Some`/`None` positions and the `Some` payload type of an
/// official `Option`. `None` carries no payload, unlike `Result`'s `Error`.
struct OptionCases {
    some_index: usize,
    none_index: usize,
    success: Type,
}

#[derive(Clone, Debug, Default)]
struct Context {
    types: HashMap<String, TypeInfo>,
    functions: HashMap<String, Vec<Callable>>,
    globals: HashMap<String, Binding>,
    /// Nominal identity (linked base name) of the official `aster.core.Result`
    /// and `Option`, discovered once from the loaded declarations. `None` when
    /// the core standard library is absent, in which case no type is official.
    official_result: Option<String>,
    official_option: Option<String>,
    /// Every callable declared `virtual`, `override` or `abstract` (object
    /// model B1, virtual dispatch). A call to one of these on `this` is
    /// dispatched dynamically unless it goes through `base`.
    virtual_callables: HashSet<CallableKey>,
    /// Each override mapped to the member it directly overrides.
    overrides: HashMap<CallableKey, CallableKey>,
    /// Overrides (own declaration keys) that found an inherited member to
    /// replace; an `override` missing from this set is an error.
    matched_overrides: HashSet<CallableKey>,
}

#[derive(Clone, Debug)]
struct Binding {
    type_: Type,
    mutable: bool,
    iteration_readonly: bool,
    initialized: bool,
    span: Span,
    /// The evaluated value of a `const` binding; `None` for variables.
    value: Option<ConstValue>,
}

/// Where analysis currently is relative to an `async` function's single
/// `await`. Replaces two coupled booleans (`is_async` + `saw_await`) so the
/// impossible "saw await outside async" combination cannot be represented.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AsyncAnalysisState {
    OutsideAsync,
    BeforeAwait,
    AfterAwait,
}

#[allow(clippy::struct_excessive_bools)]
struct Analyzer<'a> {
    context: &'a Context,
    scopes: Vec<HashMap<String, Binding>>,
    return_type: Type,
    methods: HashMap<String, Vec<Callable>>,
    owner: Option<String>,
    constructor: bool,
    instance_context: bool,
    model: &'a mut Model,
    field_names: HashSet<String>,
    diagnostics: Vec<Diagnostic>,
    loop_depth: usize,
    model_context: String,
    async_state: AsyncAnalysisState,
    unsafe_depth: usize,
    /// How the body under analysis uses `this`; `None` outside instance bodies.
    this_usage: Option<this_confinement::ThisUsage>,
    /// Analyzing the `base(...)` arguments of a constructor, where the object
    /// is not usable yet.
    base_arguments: bool,
    /// `"base"` or `"this"`: which initializer `base_arguments` belongs to.
    chain_label: &'static str,
    /// Analyzing a member reached through `base`: a static call to the base
    /// implementation, never dispatched dynamically.
    base_access: bool,
}

impl<'a> Analyzer<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        context: &'a Context,
        return_type: Type,
        fields: HashMap<String, Type>,
        methods: HashMap<String, Vec<Callable>>,
        owner: Option<String>,
        constructor: bool,
        instance_context: bool,
        initialized_fields: &HashSet<String>,
        model_context: String,
        model: &'a mut Model,
    ) -> Self {
        let owner_is_class = owner.as_ref().is_some_and(|owner| {
            context
                .types
                .get(owner)
                .is_some_and(|info| info.kind == Some(TypeKind::Class))
        });
        let mut outer = context.globals.clone();
        let field_names = fields.keys().cloned().collect();
        outer.extend(fields.into_iter().map(|(name, type_)| {
            let initialized = initialized_fields.contains(&name)
                || !constructor
                || zero_initializable(&type_, context, &mut HashSet::new());
            (
                name,
                Binding {
                    type_,
                    mutable: true,
                    iteration_readonly: false,
                    initialized,
                    span: Span::default(),
                    value: None,
                },
            )
        }));
        Self {
            context,
            scopes: vec![outer, HashMap::new()],
            return_type,
            methods,
            owner,
            constructor,
            instance_context,
            model,
            field_names,
            diagnostics: Vec::new(),
            loop_depth: 0,
            model_context,
            async_state: AsyncAnalysisState::OutsideAsync,
            unsafe_depth: 0,
            this_usage: (instance_context && owner_is_class)
                .then(this_confinement::ThisUsage::default),
            base_arguments: false,
            chain_label: "base",
            base_access: false,
        }
    }

    fn model_key(&self, span: Span) -> crate::semantic::ModelNodeKey {
        self.model.node_key(&self.model_context, span)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypeKind {
    Class,
    Struct,
    Interface,
    Enum,
}
