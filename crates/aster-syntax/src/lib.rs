//! Syntax front-end for Aster.

mod ast;
mod lexer;
mod parser;
mod token;
pub mod visit;

pub use ast::{
    Accessor, Argument, AssignmentOperator, BinaryOperator, Block, EnumCase, EnumDeclaration,
    Expression, ExpressionKind, Field, FieldInitializer, FunctionDeclaration, IncrementOperator,
    InterpolatedPart, Item, Literal, Member, Module, NamespaceDeclaration, Parameter, Property,
    Statement, SwitchCase, SwitchExpressionCase, TypeDeclaration, TypeParameter, TypeRef,
    UnaryOperator, UsingDeclaration, VariableDeclaration, VariableKind, Visibility,
};
pub use lexer::lex;
pub use parser::{MAX_SOURCE_NESTING, parse};
pub use token::{Token, TokenKind};
