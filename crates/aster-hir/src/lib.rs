//! Typed, resolved high-level intermediate representation for Aster.
//!
//! HIR contains no execution engine, machine code, ECS runtime, or backend details.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SymbolId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Void,
    Bool,
    SByte,
    Byte,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    Float,
    Double,
    Decimal,
    Char,
    String,
    User(SymbolId),
    Class(SymbolId),
    Interface(SymbolId),
    Enum(SymbolId),
    Array(Box<Type>),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Internal,
    Protected,
    Private,
}

/// Host service selected by trusted compiler metadata, never by user syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intrinsic {
    ReportRuntimeError(RuntimeErrorKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeErrorKind {
    MathAbsIntOverflow,
    MathAbsLongOverflow,
    MathClampInvalidRange,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Class(TypeDeclaration),
    Struct(TypeDeclaration),
    Interface(TypeDeclaration),
    Enum(EnumDeclaration),
    Function(Function),
    Variable(Variable),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumDeclaration {
    pub symbol: SymbolId,
    pub name: String,
    pub visibility: Visibility,
    pub cases: Vec<EnumCase>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumCase {
    pub symbol: SymbolId,
    pub name: String,
    pub tag: u32,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDeclaration {
    pub symbol: SymbolId,
    pub name: String,
    pub visibility: Visibility,
    pub interfaces: Vec<SymbolId>,
    pub fields: Vec<Field>,
    pub methods: Vec<Function>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub symbol: SymbolId,
    pub name: String,
    pub visibility: Visibility,
    pub type_: Type,
    pub initializer: Option<Expression>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub constructor: bool,
    pub is_static: bool,
    pub symbol: SymbolId,
    pub name: String,
    pub visibility: Visibility,
    pub intrinsic: Option<Intrinsic>,
    pub parameters: Vec<Parameter>,
    pub return_type: Type,
    pub body: Option<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    pub symbol: SymbolId,
    pub name: String,
    pub type_: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub symbol: SymbolId,
    pub name: String,
    pub visibility: Option<Visibility>,
    pub type_: Type,
    pub mutable: bool,
    pub initializer: Option<Expression>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub statements: Vec<Statement>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    Variable(Variable),
    Return(Option<Expression>),
    Expression(Expression),
    If {
        condition: Expression,
        then_block: Block,
        else_block: Option<Block>,
    },
    While {
        condition: Expression,
        body: Block,
    },
    For {
        initializer: Option<Box<Statement>>,
        condition: Option<Expression>,
        update: Option<Expression>,
        body: Block,
    },
    Switch {
        value: Expression,
        cases: Vec<SwitchCase>,
        default: Option<Block>,
    },
    Break,
    Continue,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwitchCase {
    pub case: SymbolId,
    pub tag: u32,
    pub bindings: Vec<Parameter>,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expression {
    pub type_: Type,
    pub kind: ExpressionKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExpressionKind {
    Literal(Literal),
    Symbol(SymbolId),
    StructLiteral {
        struct_symbol: SymbolId,
        fields: Vec<FieldValue>,
    },
    EnumValue {
        enum_symbol: SymbolId,
        case: SymbolId,
        tag: u32,
        fields: Vec<FieldValue>,
    },
    NewObject {
        class_symbol: SymbolId,
        constructor: SymbolId,
        arguments: Vec<Expression>,
    },
    ArrayLiteral(Vec<Expression>),
    NewArray {
        element_type: Type,
        length: Box<Expression>,
    },
    Index {
        array: Box<Expression>,
        index: Box<Expression>,
    },
    ArrayLength(Box<Expression>),
    /// Number of Unicode scalar values in an immutable UTF-8 string.
    StringLength(Box<Expression>),
    Member {
        object: Box<Expression>,
        symbol: SymbolId,
    },
    Call {
        callee: Box<Expression>,
        arguments: Vec<Expression>,
    },
    PropertyAssignment {
        object: Box<Expression>,
        getter: Option<SymbolId>,
        setter: SymbolId,
        operator: AssignmentOperator,
        value: Box<Expression>,
    },
    /// A standard-library logging call: `Log`, `Log.Warning`, or `Log.Error`.
    LogCall {
        level: LogLevel,
        argument: Box<Expression>,
    },
    /// A numeric or `char` conversion; the target is this expression's type.
    /// Covers both implicit widening and explicit `(type)value` casts.
    Convert {
        operand: Box<Expression>,
    },
    /// Nominal, non-owning conversion from a class reference to an interface value.
    UpcastInterface {
        object: Box<Expression>,
        class: SymbolId,
        interface: SymbolId,
    },
    Unary {
        operator: UnaryOperator,
        operand: Box<Expression>,
    },
    IncrementDecrement {
        operator: IncrementOperator,
        prefix: bool,
        target: Box<Expression>,
    },
    Conditional {
        condition: Box<Expression>,
        when_true: Box<Expression>,
        when_false: Box<Expression>,
    },
    /// Postfix `?` on an `aster.core.Result<T, E>`: evaluate `operand` once, and
    /// either continue with the `Ok` payload (this expression's `type_`, `T`) or
    /// early-return the enclosing function's `Result<U, E>.Error(error)`. Every
    /// case, field, and tag is resolved here so no later stage inspects names.
    PropagateResult {
        operand: Box<Expression>,
        success_type: Type,
        error_type: Type,
        ok_case: SymbolId,
        ok_field: SymbolId,
        ok_tag: u32,
        error_case: SymbolId,
        error_field: SymbolId,
        error_tag: u32,
        return_type: Type,
        return_error_case: SymbolId,
        return_error_field: SymbolId,
        return_error_tag: u32,
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
    /// `$"text {expr} text"`, already validated: every embedded expression
    /// has a type with a defined textual conversion. Always typed `string`.
    InterpolatedString {
        parts: Vec<InterpolatedPart>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum InterpolatedPart {
    Text(String),
    Expression(Box<Expression>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldValue {
    pub field: SymbolId,
    pub value: Expression,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    Integer(String),
    Float(String),
    /// An exact base-10 value; kept as source text because no runtime
    /// representation exists yet.
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
pub enum LogLevel {
    Log,
    Warning,
    Error,
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

impl fmt::Display for Module {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:#?}")
    }
}
