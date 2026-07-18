mod declarations;
mod expressions;
mod statements;

pub use declarations::*;
pub use expressions::*;
pub use statements::*;

use aster_diagnostics::Span;

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub namespace: Option<NamespaceDeclaration>,
    pub usings: Vec<UsingDeclaration>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceDeclaration {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsingDeclaration {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Class(TypeDeclaration),
    Struct(TypeDeclaration),
    Interface(TypeDeclaration),
    Enum(EnumDeclaration),
    Function(FunctionDeclaration),
    Variable(VariableDeclaration),
}

impl Item {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Class(item) | Self::Struct(item) | Self::Interface(item) => item.span,
            Self::Enum(item) => item.span,
            Self::Function(item) => item.span,
            Self::Variable(item) => item.span,
        }
    }
}
