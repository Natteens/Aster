//! Typed, control-flow-explicit mid-level intermediate representation for Aster.
//!
//! MIR is independent of source syntax and contains no backend, machine code,
//! execution engine, or ECS runtime behavior.

use std::fmt;

pub use aster_hir::{SymbolId, Type, Visibility};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BasicBlockId(pub u32);

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub structs: Vec<StructDefinition>,
    pub classes: Vec<ClassDefinition>,
    pub interfaces: Vec<InterfaceDefinition>,
    pub enums: Vec<EnumDefinition>,
    pub interface_implementations: Vec<InterfaceImplementation>,
    pub functions: Vec<Function>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumDefinition {
    pub symbol: SymbolId,
    pub name: String,
    pub cases: Vec<EnumCaseDefinition>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumCaseDefinition {
    pub symbol: SymbolId,
    pub name: String,
    pub tag: u32,
    pub fields: Vec<FieldDefinition>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StructDefinition {
    pub symbol: SymbolId,
    pub name: String,
    pub fields: Vec<FieldDefinition>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClassDefinition {
    pub symbol: SymbolId,
    pub name: String,
    pub fields: Vec<FieldDefinition>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceDefinition {
    pub symbol: SymbolId,
    pub name: String,
    pub methods: Vec<InterfaceMethodDefinition>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceMethodDefinition {
    pub symbol: SymbolId,
    pub name: String,
    pub parameters: Vec<Type>,
    pub return_type: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceImplementation {
    pub class: SymbolId,
    pub interface: SymbolId,
    /// Concrete method symbols in the interface declaration order.
    pub methods: Vec<SymbolId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDefinition {
    pub symbol: SymbolId,
    pub name: String,
    pub type_: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub constructor: bool,
    pub symbol: SymbolId,
    /// Owning class or struct, when this function originated as a method.
    pub owner: Option<SymbolId>,
    pub name: String,
    pub visibility: Visibility,
    pub parameters: Vec<Local>,
    pub locals: Vec<Local>,
    pub return_type: Type,
    pub entry: BasicBlockId,
    pub blocks: Vec<BasicBlock>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Local {
    pub id: LocalId,
    pub symbol: Option<SymbolId>,
    pub name: String,
    pub type_: Type,
    pub mutable: bool,
    pub temporary: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BasicBlock {
    pub id: BasicBlockId,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
}

/// Storage region selected for one dynamic allocation.
///
/// The compiler emits `Temporary` only for objects, arrays, and dynamic
/// strings proven not to escape their containing function. Every uncertain
/// allocation remains `Persistent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocationRegion {
    Persistent,
    Temporary,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Instruction {
    Assign {
        target: Place,
        value: Rvalue,
    },
    Call {
        destination: Option<Place>,
        function: SymbolId,
        arguments: Vec<Operand>,
        return_type: Type,
    },
    CallInterface {
        destination: Option<Place>,
        receiver: Operand,
        method: SymbolId,
        arguments: Vec<Operand>,
        return_type: Type,
    },
    /// Call into the Aster runtime instead of another compiled Aster function.
    CallIntrinsic {
        destination: Option<Place>,
        intrinsic: Intrinsic,
        arguments: Vec<Operand>,
        return_type: Type,
    },
    AllocateArray {
        destination: Place,
        element_type: Type,
        length: Operand,
        requires_default: bool,
        region: AllocationRegion,
    },
    AllocateObject {
        destination: Place,
        class: SymbolId,
        region: AllocationRegion,
    },
}

/// Runtime services reachable from generated code. Backends map each variant
/// to a symbol from the `aster-runtime` registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intrinsic {
    Log,
    LogWarning,
    LogError,
    StringEquals,
    StringConcat,
    StringConcatTemporary,
    StringLength,
    /// Convert a signed integer (widened to `long`) to a `string`.
    StringFromLong,
    StringFromLongTemporary,
    /// Convert an unsigned integer (widened to `ulong`) to a `string`.
    StringFromULong,
    StringFromULongTemporary,
    /// Convert a `float` (promoted to `double`) or `double` to a `string`.
    StringFromDouble,
    StringFromDoubleTemporary,
    StringFromBool,
    StringFromBoolTemporary,
    StringFromChar,
    StringFromCharTemporary,
    /// Join every argument, each already a `string`, into one new `string`,
    /// in a single allocation. Backs string interpolation.
    StringJoin,
    StringJoinTemporary,
    ReportRuntimeError(RuntimeErrorKind),
}

impl Intrinsic {
    /// Region carried by intrinsics that create a dynamic string.
    #[must_use]
    pub const fn string_allocation_region(self) -> Option<AllocationRegion> {
        match self {
            Self::StringConcat
            | Self::StringFromLong
            | Self::StringFromULong
            | Self::StringFromDouble
            | Self::StringFromBool
            | Self::StringFromChar
            | Self::StringJoin => Some(AllocationRegion::Persistent),
            Self::StringConcatTemporary
            | Self::StringFromLongTemporary
            | Self::StringFromULongTemporary
            | Self::StringFromDoubleTemporary
            | Self::StringFromBoolTemporary
            | Self::StringFromCharTemporary
            | Self::StringJoinTemporary => Some(AllocationRegion::Temporary),
            Self::Log
            | Self::LogWarning
            | Self::LogError
            | Self::StringEquals
            | Self::StringLength
            | Self::ReportRuntimeError(_) => None,
        }
    }

    /// Return the equivalent dynamic-string intrinsic for `region`.
    #[must_use]
    pub const fn with_string_allocation_region(self, region: AllocationRegion) -> Self {
        match (self, region) {
            (Self::StringConcat | Self::StringConcatTemporary, AllocationRegion::Persistent) => {
                Self::StringConcat
            }
            (Self::StringConcat | Self::StringConcatTemporary, AllocationRegion::Temporary) => {
                Self::StringConcatTemporary
            }
            (
                Self::StringFromLong | Self::StringFromLongTemporary,
                AllocationRegion::Persistent,
            ) => Self::StringFromLong,
            (Self::StringFromLong | Self::StringFromLongTemporary, AllocationRegion::Temporary) => {
                Self::StringFromLongTemporary
            }
            (
                Self::StringFromULong | Self::StringFromULongTemporary,
                AllocationRegion::Persistent,
            ) => Self::StringFromULong,
            (
                Self::StringFromULong | Self::StringFromULongTemporary,
                AllocationRegion::Temporary,
            ) => Self::StringFromULongTemporary,
            (
                Self::StringFromDouble | Self::StringFromDoubleTemporary,
                AllocationRegion::Persistent,
            ) => Self::StringFromDouble,
            (
                Self::StringFromDouble | Self::StringFromDoubleTemporary,
                AllocationRegion::Temporary,
            ) => Self::StringFromDoubleTemporary,
            (
                Self::StringFromBool | Self::StringFromBoolTemporary,
                AllocationRegion::Persistent,
            ) => Self::StringFromBool,
            (Self::StringFromBool | Self::StringFromBoolTemporary, AllocationRegion::Temporary) => {
                Self::StringFromBoolTemporary
            }
            (
                Self::StringFromChar | Self::StringFromCharTemporary,
                AllocationRegion::Persistent,
            ) => Self::StringFromChar,
            (Self::StringFromChar | Self::StringFromCharTemporary, AllocationRegion::Temporary) => {
                Self::StringFromCharTemporary
            }
            (Self::StringJoin | Self::StringJoinTemporary, AllocationRegion::Persistent) => {
                Self::StringJoin
            }
            (Self::StringJoin | Self::StringJoinTemporary, AllocationRegion::Temporary) => {
                Self::StringJoinTemporary
            }
            _ => self,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeErrorKind {
    MathAbsIntOverflow,
    MathAbsLongOverflow,
    MathClampInvalidRange,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Terminator {
    Goto(BasicBlockId),
    Branch {
        condition: Operand,
        then_block: BasicBlockId,
        else_block: BasicBlockId,
    },
    Return(Option<Operand>),
    End,
    Unreachable,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    Local(LocalId),
    Symbol(SymbolId),
    Field {
        base: Box<Place>,
        field: SymbolId,
    },
    Index {
        array: Box<Operand>,
        index: Box<Operand>,
        element_type: Type,
    },
    ObjectField {
        object: Box<Operand>,
        field: SymbolId,
    },
    EnumField {
        base: Box<Place>,
        case: SymbolId,
        field: SymbolId,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Operand {
    pub type_: Type,
    pub kind: OperandKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OperandKind {
    Constant(Constant),
    Copy(Place),
    Function(SymbolId),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rvalue {
    pub type_: Type,
    pub kind: RvalueKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RvalueKind {
    Use(Operand),
    Aggregate(Vec<FieldOperand>),
    EnumConstruct {
        case: SymbolId,
        tag: u32,
        fields: Vec<FieldOperand>,
    },
    Discriminant(Operand),
    ArrayLength(Operand),
    /// Convert the operand to this rvalue's type.
    Cast(Operand),
    MakeInterface {
        object: Operand,
        class: SymbolId,
        interface: SymbolId,
    },
    Unary {
        operator: UnaryOperator,
        operand: Operand,
    },
    Binary {
        left: Operand,
        operator: BinaryOperator,
        right: Operand,
    },
    Equality {
        left: Operand,
        right: Operand,
        negated: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldOperand {
    pub field: SymbolId,
    pub value: Operand,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Integer(String),
    Float(String),
    /// An exact base-10 value; rejected by backends until a decimal runtime
    /// representation exists.
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

impl fmt::Display for Module {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:#?}")
    }
}
