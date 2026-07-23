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
    /// `aster.core.Task<T>`: an opaque host-side handle for one spawned task.
    /// Compiler-intrinsic, like `Array`; never monomorphized like a stdlib
    /// generic, and never exposes arena identity to generated code.
    Task(Box<Type>),
    /// `List<T>`: a native reference type with its own runtime header and
    /// growable buffer (see `aster_runtime::AsterList`). Compiler-intrinsic,
    /// like `Array`/`Task`; the element type `T` must already be concrete
    /// and executable when this variant exists.
    List(Box<Type>),
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
    /// `aster.io.Write(string)`: emits its argument's UTF-8 bytes verbatim.
    ConsoleWrite,
    /// `aster.io.WriteLine(string)`: like `ConsoleWrite`, plus a trailing LF.
    ConsoleWriteLine,
    /// `aster.io.ReadLine()`: reads one line into `Option<string>`; `None` on EOF.
    ConsoleReadLine,
    /// `aster.io.ReadAllText(string) -> Result<string, IOError>`: reads an
    /// entire UTF-8 text file via the host filesystem backend. The payload
    /// is resolved once, here in HIR lowering (never by the backend), from
    /// the official `aster.core.Result<string, IOError>`/`aster.io.IOError`/
    /// `aster.io.IOErrorKind` declarations -- see [`FileIoResultLayout`].
    FileReadAllText(FileIoResultLayout),
    /// `aster.io.WriteAllText(string, string) -> Result<int, IOError>`:
    /// creates or truncates a file and writes UTF-8 text via the host
    /// filesystem backend. Payload resolved the same way as
    /// [`Self::FileReadAllText`], against `Result<int, IOError>`.
    FileWriteAllText(FileIoResultLayout),
    /// `aster.io.ListFiles(string) -> Result<string[], IOError>`: lists the
    /// immediately contained regular files through the host filesystem
    /// backend. The concrete `Result<string[], IOError>` layout is resolved
    /// exactly once before MIR; the backend receives symbols, never names.
    FileListFiles(FileIoResultLayout),
}

/// Every concrete symbol the M2D filesystem intrinsics need to construct a
/// `Result<T, IOError>` value, resolved exactly once during HIR lowering
/// (from the official stdlib declarations' own, ordinary case/field
/// resolution -- the same mechanism `ExpressionKind::PropagateResult`'s
/// `ok_case`/`ok_field`/`error_case`/`error_field` already use for postfix
/// `?`) and carried as plain data from then on. The backend looks these up in
/// its own layout tables strictly by `SymbolId` equality; it never compares
/// case, field, or variant names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIoResultLayout {
    /// `Result<T, IOError>`'s `Ok` case.
    pub ok_case: SymbolId,
    /// `Ok`'s single payload field (of type `T`).
    pub ok_field: SymbolId,
    /// `Result<T, IOError>`'s `Error` case.
    pub error_case: SymbolId,
    /// `Error`'s single payload field (of type `IOError`).
    pub error_field: SymbolId,
    /// `IOError.Kind`.
    pub io_error_kind_field: SymbolId,
    /// `IOError.OsCode`.
    pub io_error_os_code_field: SymbolId,
    /// `IOErrorKind`'s 9 cases, indexed in the fixed order
    /// `aster_runtime::PortableIoErrorKind`'s variants declare (`NotFound`,
    /// `PermissionDenied`, `AlreadyExists`, `InvalidPath`, `InvalidUtf8`,
    /// `NotFile`, `NotDirectory`, `LimitExceeded`, `Other`).
    pub portable_kind_cases: [SymbolId; 9],
}

impl FileIoResultLayout {
    /// Sentinel used only as a placeholder in
    /// `StandardLibrary::intrinsic_bindings()`, before HIR lowering resolves
    /// the real symbols; never reaches MIR or the backend.
    pub const UNRESOLVED: Self = Self {
        ok_case: SymbolId(0),
        ok_field: SymbolId(0),
        error_case: SymbolId(0),
        error_field: SymbolId(0),
        io_error_kind_field: SymbolId(0),
        io_error_os_code_field: SymbolId(0),
        portable_kind_cases: [SymbolId(0); 9],
    };
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
    pub is_async: bool,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringOperation {
    Contains,
    StartsWith,
    EndsWith,
    IndexOf,
    SubstringFrom,
    SubstringRange,
    /// `text.TryParseBool()`/`TryParseInt()`/`TryParseUInt()`/`TryParseLong()`/
    /// `TryParseULong()`/`TryParseFloat()`/`TryParseDouble()`: deterministic,
    /// allocation-free parsing into the official `Option<T>` for the
    /// matching primitive. Takes no arguments.
    TryParseBool,
    TryParseInt,
    TryParseUInt,
    TryParseLong,
    TryParseULong,
    TryParseFloat,
    TryParseDouble,
}

impl StringOperation {
    /// The primitive name (as spelled in Aster source) `TryParse*` targets.
    /// `None` for every non-parsing operation.
    #[must_use]
    pub const fn parse_target_name(self) -> Option<&'static str> {
        match self {
            Self::TryParseBool => Some("bool"),
            Self::TryParseInt => Some("int"),
            Self::TryParseUInt => Some("uint"),
            Self::TryParseLong => Some("long"),
            Self::TryParseULong => Some("ulong"),
            Self::TryParseFloat => Some("float"),
            Self::TryParseDouble => Some("double"),
            Self::Contains
            | Self::StartsWith
            | Self::EndsWith
            | Self::IndexOf
            | Self::SubstringFrom
            | Self::SubstringRange => None,
        }
    }
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
    /// `new List<T>()`: allocates an empty `List<T>` header (see
    /// `aster_runtime::AsterList`). `element_type` is already concrete.
    NewList {
        element_type: Type,
    },
    Index {
        array: Box<Expression>,
        index: Box<Expression>,
    },
    ArrayLength(Box<Expression>),
    /// Number of Unicode scalar values in an immutable UTF-8 string.
    StringLength(Box<Expression>),
    /// One resolved built-in `string` operation. The receiver and arguments
    /// remain separate so lowering preserves receiver-first evaluation.
    StringOperation {
        operation: StringOperation,
        receiver: Box<Expression>,
        arguments: Vec<Expression>,
    },
    /// `value.ToString()`: canonical, deterministic, culture-invariant textual
    /// conversion of one of the eight fundamental primitives (`bool`, `char`,
    /// `int`, `uint`, `long`, `ulong`, `float`, `double`). `primitive` is
    /// always `receiver`'s own concrete type, never a textual type name, so
    /// the backend never guesses which runtime conversion to call.
    FormatPrimitive {
        primitive: Type,
        receiver: Box<Expression>,
    },
    /// `list.Length`: reads the `length` field of a `List<T>` header.
    ListLength(Box<Expression>),
    /// `list.Add(value)`: appends `value`, growing the buffer if needed.
    /// Always typed `void`. `value`'s type is exactly the list's element
    /// type (already converted, if needed, by the time this is built).
    ListAdd {
        list: Box<Expression>,
        value: Box<Expression>,
    },
    /// `list.Get(index)`: always produces a value copy, never a pointer
    /// into the list's buffer. `element_type` is this expression's own
    /// `type_`, restated here structurally instead of re-derived by name.
    ListGet {
        list: Box<Expression>,
        index: Box<Expression>,
        element_type: Type,
    },
    /// `list.RemoveAt(index)`: always typed `void`.
    ListRemoveAt {
        list: Box<Expression>,
        index: Box<Expression>,
    },
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
    /// Postfix `?` on the official `aster.core.Option<T>`: evaluate `operand`
    /// once, and either continue with the `Some` payload (this expression's
    /// `type_`, `T`) or early-return the enclosing function's
    /// `Option<U>.None` -- `U` need not equal `T`. Every case, field, and tag
    /// is resolved here so no later stage inspects names.
    PropagateOption {
        operand: Box<Expression>,
        success_type: Type,
        some_case: SymbolId,
        some_field: SymbolId,
        some_tag: u32,
        none_tag: u32,
        return_type: Type,
        return_none_case: SymbolId,
        return_none_tag: u32,
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
    /// `aster.core.Task.Run(function)`: spawn a statically resolved,
    /// zero-parameter free function or static method on the host's
    /// execution pool. `function` is never a variable, a generic template,
    /// or an interface dispatch; semantic analysis resolves it to a concrete
    /// symbol before this node exists. This expression's type is always
    /// `Type::Task(return_type)`.
    TaskRun {
        function: SymbolId,
        return_type: Box<Type>,
    },
    /// `task.Wait()` on an `aster.core.Task<T>` value: block until the task
    /// completes and produce its `T` result, or propagate a controlled
    /// error. This expression's type is always `result_type` (`T`).
    TaskWait {
        task: Box<Expression>,
        result_type: Box<Type>,
    },
    /// `await operand` inside an `async` function: `operand` is a `Task<T>`
    /// expression (only `Task.Run(...)` in this version) and this node's type
    /// is the concrete scalar `result_type` (`T`).
    Await {
        operand: Box<Expression>,
        result_type: Box<Type>,
    },
    /// `Parallel.For(start, end, Body)`: run `body` over `[start, end)` on the
    /// host worker pool and block until every chunk finishes. `body` is a
    /// resolved, zero-capture free function or static method taking one `int`
    /// and returning `void`. This expression's type is always `void`.
    ParallelFor {
        start: Box<Expression>,
        end: Box<Expression>,
        body: SymbolId,
    },
    /// `Parallel.ForEach(values, Body)`: run `body` over every element of the
    /// scalar array `values`. `element_type` is the concrete scalar element
    /// type; `body` takes one `element_type` and returns `void`. This
    /// expression's type is always `void`.
    ParallelForEach {
        values: Box<Expression>,
        element_type: Box<Type>,
        body: SymbolId,
    },
    /// `Parallel.Reduce(values, identity, Accumulate, Combine)`: fold
    /// `Accumulate` over every element of the scalar array `values`, starting
    /// from `identity`, then combine chunk partials with `Combine`.
    /// `element_type` is the concrete scalar element type (`TElement`);
    /// `identity`'s type is the concrete accumulator type (`TAccumulator`).
    /// This expression's type is always `TAccumulator`.
    ParallelReduce {
        values: Box<Expression>,
        element_type: Box<Type>,
        identity: Box<Expression>,
        accumulate: SymbolId,
        combine: SymbolId,
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
