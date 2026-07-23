use aster_diagnostics::Span;

use super::{Expression, TypeRef, VariableDeclaration};

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub statements: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    Variable(VariableDeclaration),
    Return {
        value: Option<Expression>,
        span: Span,
    },
    If {
        condition: Expression,
        then_block: Block,
        else_block: Option<Block>,
        span: Span,
    },
    While {
        condition: Expression,
        body: Block,
        span: Span,
    },
    For {
        initializer: Option<Box<Statement>>,
        condition: Option<Expression>,
        update: Option<Expression>,
        body: Block,
        span: Span,
    },
    ForEach {
        element_type: TypeRef,
        element_name: String,
        collection: Expression,
        body: Block,
        span: Span,
    },
    Switch {
        value: Expression,
        cases: Vec<SwitchCase>,
        default: Option<Block>,
        span: Span,
    },
    Break(Span),
    Continue(Span),
    Expression(Expression),
}

impl Statement {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Variable(statement) => statement.span,
            Self::Return { span, .. }
            | Self::If { span, .. }
            | Self::While { span, .. }
            | Self::For { span, .. }
            | Self::ForEach { span, .. }
            | Self::Switch { span, .. }
            | Self::Break(span)
            | Self::Continue(span) => *span,
            Self::Expression(expression) => expression.span,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwitchCase {
    pub enum_name: Option<String>,
    pub case_name: String,
    pub bindings: Vec<String>,
    pub body: Block,
    pub span: Span,
}
