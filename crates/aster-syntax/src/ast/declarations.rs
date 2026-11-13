use aster_diagnostics::Span;

use super::{Argument, Block, Expression};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Internal,
    Protected,
    Private,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeRef {
    pub name: String,
    pub span: Span,
}

impl TypeRef {
    #[must_use]
    pub fn new(name: impl Into<String>, span: Span) -> Self {
        Self {
            name: name.into(),
            span,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDeclaration {
    pub visibility: Visibility,
    pub is_static: bool,
    pub name: String,
    pub type_parameters: Vec<TypeParameter>,
    /// Class declared `abstract`: it cannot be instantiated. Always `false` for
    /// structs and interfaces (the parser rejects the modifier there).
    pub is_abstract: bool,
    /// Class declared `sealed`: no class may derive from it.
    pub is_sealed: bool,
    /// The names after `:` in declaration order. Semantic analysis classifies
    /// each name: a class base (at most one, and it must be first) or an
    /// interface. Empty for structs and interfaces.
    pub interfaces: Vec<TypeRef>,
    pub members: Vec<Member>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumDeclaration {
    pub visibility: Visibility,
    pub name: String,
    pub type_parameters: Vec<TypeParameter>,
    pub cases: Vec<EnumCase>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumCase {
    pub name: String,
    pub fields: Vec<Parameter>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Member {
    Field(Field),
    Method(FunctionDeclaration),
    Property(Property),
}

/// Dispatch modifier of a class method or property (object model B1, virtual
/// dispatch). `sealed override` is deliberately not representable: exact
/// signatures and no `sealed override` in B1 (U8).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MemberDispatch {
    /// Statically bound (the default).
    #[default]
    None,
    /// Introduces a virtual slot.
    Virtual,
    /// Replaces an inherited virtual, abstract or overriding member.
    Override,
    /// Introduces a virtual slot without an implementation; the declaring
    /// class must be abstract.
    Abstract,
}

/// The name of the synthesized private backing field of auto-property
/// `property` (LANG-005). It contains `<`, so no source identifier can name it.
#[must_use]
pub fn auto_property_backing_field(property: &str) -> String {
    format!("<{property}>k__BackingField")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub dispatch: MemberDispatch,
    pub visibility: Visibility,
    pub type_ref: TypeRef,
    pub name: String,
    pub getter: Option<Accessor>,
    pub setter: Option<Accessor>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Accessor {
    pub visibility: Visibility,
    pub explicit_visibility: bool,
    /// An `init` accessor (LANG-005): a setter assignable only while the
    /// object is being constructed. Never set on a getter.
    pub is_init: bool,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub visibility: Visibility,
    pub type_ref: TypeRef,
    pub name: String,
    pub initializer: Option<Expression>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct FunctionDeclaration {
    pub constructor: bool,
    /// `virtual`, `override` or `abstract` on a class method; `None` for every
    /// other function.
    pub dispatch: MemberDispatch,
    /// Marks a namespace-level test callable. This is compiler metadata only:
    /// its body still has ordinary ASTER function semantics.
    pub is_test: bool,
    pub is_static: bool,
    pub is_async: bool,
    /// Host-provided native declaration. Foreign functions are bodyless,
    /// free functions and are executable only from a lexical `unsafe` block.
    pub is_foreign: bool,
    pub type_parameters: Vec<TypeParameter>,
    pub visibility: Visibility,
    pub return_type: TypeRef,
    pub name: String,
    pub parameters: Vec<Parameter>,
    /// The `: base(arguments)` initializer of a constructor. `None` for every
    /// other function and for a constructor that omits it.
    pub base_call: Option<BaseCall>,
    pub body: Option<Block>,
    pub span: Span,
}

/// `: base(arguments)` or `: this(arguments)` after a constructor's parameter
/// list.
#[derive(Clone, Debug, PartialEq)]
pub struct BaseCall {
    /// `true` for `this(...)`, which delegates to another constructor of the
    /// same class; `false` for `base(...)`.
    pub delegates_to_this: bool,
    pub arguments: Vec<Argument>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeParameter {
    pub name: String,
    pub span: Span,
    /// Interfaces this parameter is required to satisfy, from a trailing
    /// `where` clause. Namespace linking rewrites these to linked nominal
    /// names, and specialization clears them with the rest of the parameter
    /// list, so no constraint outlives monomorphization.
    pub constraints: Vec<TypeRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    pub type_ref: TypeRef,
    pub name: String,
    /// Compile-time call-site default. Semantic analysis validates that this
    /// is a constant compatible with `type_ref`; it never reaches the runtime
    /// as declaration metadata.
    pub default: Option<Expression>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariableDeclaration {
    pub visibility: Option<Visibility>,
    pub kind: VariableKind,
    pub name: String,
    pub initializer: Option<Expression>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VariableKind {
    Explicit(TypeRef),
    Inferred,
    Constant(TypeRef),
}
