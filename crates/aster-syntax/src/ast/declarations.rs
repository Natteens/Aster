use aster_diagnostics::Span;

use super::{Block, Expression};

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
    /// Interfaces implemented nominally by a class. Empty for structs and interfaces.
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

#[derive(Clone, Debug, PartialEq)]
pub struct Property {
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
    pub body: Option<Block>,
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
