use aster_diagnostics::Span;

#[derive(Clone, Debug, PartialEq)]
pub struct Expression {
    pub kind: ExpressionKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExpressionKind {
    Literal(Literal),
    Name(String),
    This,
    StructLiteral {
        type_name: String,
        fields: Vec<FieldInitializer>,
    },
    ArrayLiteral(Vec<Expression>),
    NewArray {
        element_type: super::TypeRef,
        length: Box<Expression>,
    },
    NewObject {
        type_name: String,
        arguments: Vec<Expression>,
    },
    Index {
        array: Box<Expression>,
        index: Box<Expression>,
    },
    Member {
        object: Box<Expression>,
        name: String,
    },
    Call {
        callee: Box<Expression>,
        type_arguments: Vec<super::TypeRef>,
        arguments: Vec<Expression>,
    },
    Unary {
        operator: UnaryOperator,
        operand: Box<Expression>,
    },
    IncrementDecrement {
        operator: IncrementOperator,
        prefix: bool,
        operand: Box<Expression>,
    },
    /// Postfix `?`: propagate the `Error` case of an `aster.core.Result<T, E>`,
    /// continuing with the `Ok` payload. This is the only meaning of a trailing
    /// `?` today; ternary `?:`, `?.`, and `??` are separate constructs.
    Try {
        operand: Box<Expression>,
    },
    Conditional {
        condition: Box<Expression>,
        when_true: Box<Expression>,
        when_false: Box<Expression>,
    },
    /// Explicit conversion: `(type)expression`.
    Cast {
        target: super::TypeRef,
        operand: Box<Expression>,
    },
    Binary {
        left: Box<Expression>,
        operator: BinaryOperator,
        right: Box<Expression>,
    },
    Assignment {
        target: Box<Expression>,
        operator: AssignmentOperator,
        value: Box<Expression>,
    },
    /// `$"text {expr} text"`. Always produces `string`.
    InterpolatedString {
        parts: Vec<InterpolatedPart>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum InterpolatedPart {
    /// A literal run of text with escapes and `{{`/`}}` already resolved.
    Text(String),
    /// An embedded expression, parsed with the ordinary expression grammar.
    Expression(Expression),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldInitializer {
    pub name: String,
    pub value: Expression,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    /// An unsuffixed decimal integer literal. Typed `int` when the value fits,
    /// otherwise `long`.
    Integer(String),
    /// An integer literal with the `L` suffix.
    Long(String),
    /// An integer literal with the `u` suffix. Typed `uint` when the value
    /// fits, otherwise `ulong`.
    UInt(String),
    /// An integer literal with the `ul`/`lu` suffix.
    ULong(String),
    /// A decimal literal, optionally with the `f` suffix.
    Float(String),
    /// A decimal literal with the `d` suffix.
    Double(String),
    /// A base-10 exact literal with the `m` suffix.
    Decimal(String),
    String(String),
    Character(char),
    Boolean(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOperator {
    Not,
    Negate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncrementOperator {
    Increment,
    Decrement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOperator {
    Multiply,
    Divide,
    Remainder,
    Add,
    Subtract,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    LogicalAnd,
    LogicalOr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignmentOperator {
    Assign,
    AddAssign,
    SubtractAssign,
    MultiplyAssign,
    DivideAssign,
}
