use std::collections::{HashMap, HashSet};

use aster_diagnostics::{Diagnostic, Span};

use crate::constexpr::{ConstError, ConstValue, evaluate, integer_value};
use crate::primitives::{
    self, IntegerFit, Primitive, UnsignedFit, classify_integer, classify_unsigned, fits_long,
    fits_ulong,
};
use aster_syntax::{
    AssignmentOperator, BinaryOperator, Block, Expression, ExpressionKind, Field,
    FunctionDeclaration, IncrementOperator, Item, Literal, Member, Module, Property, Statement,
    TypeDeclaration, TypeRef, UnaryOperator, VariableDeclaration, VariableKind, Visibility,
};

use super::{
    AccessorKind, CallableKey, Dispatch, Model, ResolvedCall, ResolvedEnumCase,
    ResolvedPropagation, ResolvedPropertyAssignment, callable_key,
};
use crate::type_names::TypeName;

mod calls;
mod declarations;
mod expressions;
mod statements;
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
    declarations::validate_interface_implementations(module, &context, diagnostics);
    declarations::validate_struct_cycles(module, &context, diagnostics);
    declarations::validate_module_variables(module, &mut context, diagnostics, model);
    declarations::validate_bodies(module, &context, diagnostics, model);
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    parameters: Vec<Type>,
    result: Type,
}

#[derive(Clone, Debug)]
struct Callable {
    signature: Signature,
    visibility: Visibility,
    is_static: bool,
    key: CallableKey,
}

#[derive(Clone, Debug)]
struct PropertyInfo {
    type_: Type,
    getter: Option<Callable>,
    setter: Option<Callable>,
}

#[derive(Clone, Debug, Default)]
struct TypeInfo {
    is_static: bool,
    fields: HashMap<String, Type>,
    field_order: Vec<String>,
    field_visibility: HashMap<String, Visibility>,
    methods: HashMap<String, Vec<Callable>>,
    properties: HashMap<String, PropertyInfo>,
    constructor: Option<Callable>,
    constructor_visibility: Option<Visibility>,
    implemented_interfaces: Vec<String>,
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
}

#[derive(Clone, Debug)]
struct Binding {
    type_: Type,
    mutable: bool,
    initialized: bool,
    span: Span,
    /// The evaluated value of a `const` binding; `None` for variables.
    value: Option<ConstValue>,
}

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
        }
    }

    fn model_key(&self, span: Span) -> crate::semantic::ModelNodeKey {
        crate::semantic::ModelNodeKey {
            context: self.model_context.clone(),
            span,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypeKind {
    Class,
    Struct,
    Interface,
    Enum,
}
