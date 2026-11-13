//! Syntax front-end for Aster.

mod ast;
pub mod format;
mod lexer;
mod parser;
mod token;
pub mod visit;

pub use ast::{
    Accessor, Argument, AssignmentOperator, BaseCall, BinaryOperator, Block, EnumCase,
    EnumDeclaration, Expression, ExpressionKind, Field, FieldInitializer, FunctionDeclaration,
    IncrementOperator, InterpolatedPart, Item, Literal, Member, MemberDispatch, MemberInitializer,
    Module, NamespaceDeclaration, Parameter, PatternBinding, Property, Statement, SwitchCase,
    SwitchExpressionCase, TypeDeclaration, TypeParameter, TypeRef, UnaryOperator, UsingDeclaration,
    VariableDeclaration, VariableKind, Visibility, auto_property_backing_field,
};
pub use lexer::lex;
pub use parser::{MAX_SOURCE_NESTING, parse};
pub use token::{Token, TokenKind};
