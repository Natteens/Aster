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
    /// `do { body } while (condition);` — the body runs before the first test.
    DoWhile {
        body: Block,
        condition: Expression,
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
        /// `None` represents `var`; semantic analysis derives the concrete
        /// element type from the iterable before HIR is produced.
        element_type: Option<TypeRef>,
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
    Unsafe {
        body: Block,
        span: Span,
    },
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
            | Self::DoWhile { span, .. }
            | Self::For { span, .. }
            | Self::ForEach { span, .. }
            | Self::Switch { span, .. }
            | Self::Unsafe { span, .. }
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
    /// The label as an expression: always set for non-name labels (then
    /// `case_name` is empty), and set for payload-free enum-style names so a
    /// constant name can label an integer, `char` or `string` switch.
    pub label: Option<Expression>,
    /// `when` guard: the case is selected only when the pattern matches and
    /// the guard is true; otherwise matching continues with later cases.
    pub guard: Option<Expression>,
    pub body: Block,
    pub span: Span,
}
