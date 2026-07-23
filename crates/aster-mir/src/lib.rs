//! Typed, control-flow-explicit mid-level intermediate representation for Aster.
//!
//! MIR is independent of source syntax and contains no backend, machine code,
//! execution engine, or ECS runtime behavior.

use std::fmt;

pub use aster_hir::{
    DictionaryEntryLayout, DictionaryOptionLayout, FileIoResultLayout, SymbolId, Type, Visibility,
};

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
    /// Allocate an empty `List<T>` header (`length = capacity = 0`, no
    /// buffer yet) in `region`'s arena. `element_type` must be a concrete,
    /// executable type; the backend derives size/alignment/type key from it
    /// via the same layout system every other type uses — never a second,
    /// divergent list of types.
    AllocateList {
        destination: Place,
        element_type: Type,
        region: AllocationRegion,
    },
    /// Allocate an empty `Dictionary<K, V>` header. Buckets and entries are
    /// created lazily by the first insertion; hash seeds remain per-context.
    AllocateDictionary {
        destination: Place,
        key_type: Type,
        value_type: Type,
        region: AllocationRegion,
    },
    DictionaryAdd {
        destination: Place,
        dictionary: Operand,
        key: Operand,
        value: Operand,
    },
    DictionarySet {
        destination: Place,
        dictionary: Operand,
        key: Operand,
        value: Operand,
    },
    DictionaryTryGet {
        destination: Place,
        dictionary: Operand,
        key: Operand,
        value_type: Type,
        option_layout: DictionaryOptionLayout,
    },
    DictionaryContainsKey {
        destination: Place,
        dictionary: Operand,
        key: Operand,
    },
    DictionaryRemove {
        destination: Place,
        dictionary: Operand,
        key: Operand,
    },
    DictionaryEntries {
        destination: Place,
        dictionary: Operand,
        key_type: Type,
        value_type: Type,
        entry_type: Type,
        entry_layout: DictionaryEntryLayout,
        region: AllocationRegion,
    },
    /// `list.Add(value)`: appends `value` to `list`'s buffer, growing it if
    /// needed. `list`'s type is always `List(element)` for some concrete
    /// `element`, and `value.type_` is always exactly that `element` type;
    /// nothing else is stored here; the backend derives size/align/type key
    /// from `value.type_` (or, symmetrically, from `list.type_`'s inner
    /// element), exactly like `AllocateList`.
    ListAdd {
        list: Operand,
        value: Operand,
    },
    /// `list.Get(index)`: copies element `index` into `destination` — never
    /// a pointer into the buffer. `element_type` is carried explicitly
    /// (mirrors `AllocateList`) since `destination` alone cannot express the
    /// expected type; `list.type_` must equal `List(element_type)` and
    /// `destination`'s declared type must equal `element_type` exactly.
    ListGet {
        destination: Place,
        list: Operand,
        index: Operand,
        element_type: Type,
    },
    /// `list.RemoveAt(index)`: shifts every element after `index` one slot
    /// left and decrements `length`. Never touches `capacity`/`data`. The
    /// element type is derivable from `list.type_` (must be `List(T)`), so
    /// it is not stored redundantly here, exactly like `ListAdd`.
    ListRemoveAt {
        list: Operand,
        index: Operand,
    },
    /// Decodes exactly one Unicode scalar value at byte offset `cursor` in
    /// `string`'s UTF-8 payload: writes the scalar to `char_destination`
    /// (always `Type::Char`) and the resulting next cursor to
    /// `next_cursor_destination` (always `Type::Int`), and reports whether
    /// decoding succeeded via `ok_destination` (always `Type::Bool`).
    /// Reads/validates at most 4 bytes -- never rescans from the start of
    /// the string -- so `foreach`'s cursor lowering over `string` stays
    /// O(total bytes). On failure, `ExecutionContext::fail` has already
    /// been called and neither `char_destination` nor
    /// `next_cursor_destination` is written; generated code must branch on
    /// `ok_destination` itself rather than assume the loop should continue.
    /// Never a public Aster API.
    StringDecodeNext {
        string: Operand,
        cursor: Operand,
        char_destination: Place,
        next_cursor_destination: Place,
        ok_destination: Place,
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
    StringContains,
    StringStartsWith,
    StringEndsWith,
    StringIndexOf,
    StringSubstringFrom,
    StringSubstringFromTemporary,
    StringSubstringRange,
    StringSubstringRangeTemporary,
    /// Convert a signed integer (widened to `long`) to a `string`.
    StringFromLong,
    StringFromLongTemporary,
    /// Convert an unsigned integer (widened to `ulong`) to a `string`.
    StringFromULong,
    StringFromULongTemporary,
    /// Convert a `double` to a `string`.
    StringFromDouble,
    StringFromDoubleTemporary,
    /// Convert a `float` directly to a `string` (never promoted to `double`
    /// first, which would round twice and could format an already-in-range
    /// `float` incorrectly).
    StringFromFloat,
    StringFromFloatTemporary,
    StringFromBool,
    StringFromBoolTemporary,
    StringFromChar,
    StringFromCharTemporary,
    /// Join every argument, each already a `string`, into one new `string`,
    /// in a single allocation. Backs string interpolation.
    StringJoin,
    StringJoinTemporary,
    ReportRuntimeError(RuntimeErrorKind),
    /// Reports a controlled runtime failure when `foreach` over a `List<T>`
    /// detects the list was structurally modified (`Add`/`RemoveAt`) since
    /// the loop captured its version. Takes no arguments and never has a
    /// destination; like `ReportRuntimeError`, `context.fail` alone does not
    /// unwind, so lowering follows this call with an ordinary `Goto` to the
    /// loop's exit block rather than fabricating a function-level return.
    ListVersionMismatch,
    /// `aster.core.Task.Run(function)`. The sole argument is always an
    /// `OperandKind::Function` operand naming the resolved, zero-parameter
    /// target; `return_type` is that function's return type.
    TaskRun,
    /// `task.Wait()`. The sole argument is the `Task<T>` operand to join.
    TaskWait,
    /// Register one async state machine and return its `Task<T>` handle
    /// (a plain `i64`) without running its body. Arguments:
    /// `[Function(move_next_symbol), Constant(frame_slot_count)]`. Emitted
    /// only inside a generated async wrapper.
    AsyncSpawn,
    /// Read the current state (`0` or `1`) of the async task named by the
    /// hidden handle argument. Argument: `[handle]`; result `int`.
    AsyncState,
    /// Set the state of the async task named by the handle. Arguments:
    /// `[handle, Constant(state)]`.
    AsyncSetState,
    /// Store a scalar into a frame slot of the async task. Arguments:
    /// `[handle, Constant(slot_index), value]`; the ABI symbol is chosen by
    /// the value's concrete scalar type.
    AsyncStoreSlot,
    /// Load a scalar from a frame slot. Arguments: `[handle,
    /// Constant(slot_index)]`; the ABI symbol is chosen by `return_type`.
    AsyncLoadSlot,
    /// Submit the awaited `Task.Run` target to the pool with a completion
    /// token bound to this async task. Arguments:
    /// `[handle, Function(inner_symbol)]`.
    AsyncSpawnInner,
    /// Materialize the completed inner task's scalar result. Argument:
    /// `[handle]`; the ABI symbol is chosen by `return_type`.
    AsyncAwaitResult,
    /// Store the async task's candidate scalar result. Arguments:
    /// `[handle, value]`; the ABI symbol is chosen by the value's type.
    AsyncSetResult,
    /// `Parallel.For(startInclusive, endExclusive, Body)`. Arguments:
    /// `[start, end, Function(body_symbol)]`; synchronous, returns void.
    ParallelFor,
    /// `Parallel.ForEach(values, Body)`. Arguments:
    /// `[array, Function(body_symbol)]`; synchronous, returns void.
    ParallelForEach,
    /// `Parallel.Reduce(values, identity, Accumulate, Combine)`. Arguments:
    /// `[array, identity, Function(accumulate_symbol),
    /// Function(combine_symbol)]`; synchronous, returns the concrete
    /// `TAccumulator` result (`identity`'s type). `accumulate_symbol` resolves
    /// `Accumulate(TAccumulator, TElement) -> TAccumulator`; `combine_symbol`
    /// resolves `Combine(TAccumulator, TAccumulator) -> TAccumulator`.
    ParallelReduce,
    /// `text.TryParseBool()`/`TryParseInt()`/`TryParseUInt()`/`TryParseLong()`/
    /// `TryParseULong()`. Argument: `[receiver]` (a `string`); `return_type`
    /// is always the concrete `Option<T>` specialization matching the target
    /// primitive. Writes the complete enum representation (tag and, on
    /// success, payload) directly into the destination; never returns a bare
    /// scalar or a partially-initialized enum.
    StringTryParseBool,
    StringTryParseInt,
    StringTryParseUInt,
    StringTryParseLong,
    StringTryParseULong,
    StringTryParseFloat,
    StringTryParseDouble,
    /// `aster.io.Write(string)`: writes its argument's UTF-8 bytes verbatim
    /// to the console backend and flushes. Argument: `[value]` (a `string`,
    /// only ever borrowed); `return_type` always `void`.
    ConsoleWrite,
    /// `aster.io.WriteLine(string)`: like `ConsoleWrite`, plus a trailing LF.
    ConsoleWriteLine,
    /// `aster.io.ReadLine()`: reads one line from the console backend into a
    /// freshly allocated persistent `Option<string>`. No arguments;
    /// `return_type` is always the `Option<string>` specialization. `None`
    /// on EOF.
    ConsoleReadLine,
    /// Temporary-arena counterpart of [`Self::ConsoleReadLine`].
    ConsoleReadLineTemporary,
    /// `aster.io.ReadAllText(string)`: reads an entire file via the host
    /// filesystem backend into a freshly allocated persistent
    /// `Result<string, IOError>`. Argument: `[path]` (a `string`, only ever
    /// borrowed); `return_type` is always the `Result<string, IOError>`
    /// specialization. The payload's symbols are resolved once during HIR
    /// lowering (see [`FileIoResultLayout`]); this backend never looks
    /// anything up by name.
    FileReadAllText(FileIoResultLayout),
    /// Temporary-arena counterpart of [`Self::FileReadAllText`].
    FileReadAllTextTemporary(FileIoResultLayout),
    /// `aster.io.WriteAllText(string, string)`: creates or truncates the file
    /// at `path` and writes `content`'s UTF-8 bytes in full. Arguments:
    /// `[path, content]` (both only ever borrowed); `return_type` is always
    /// the `Result<int, IOError>` specialization (bytes written on success).
    /// No string escapes this intrinsic, so it has no temporary counterpart.
    FileWriteAllText(FileIoResultLayout),
    /// `aster.io.ListFiles(string)`: lists direct regular-file paths through
    /// the host filesystem backend. Both the returned `string[]` and every
    /// contained string are allocated in this single selected region.
    FileListFiles(FileIoResultLayout),
    /// Temporary-arena counterpart of [`Self::FileListFiles`].
    FileListFilesTemporary(FileIoResultLayout),
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
            | Self::StringFromFloat
            | Self::StringFromBool
            | Self::StringFromChar
            | Self::StringJoin
            | Self::StringSubstringFrom
            | Self::StringSubstringRange
            | Self::ConsoleReadLine
            | Self::FileReadAllText(_) => Some(AllocationRegion::Persistent),
            Self::StringConcatTemporary
            | Self::StringFromLongTemporary
            | Self::StringFromULongTemporary
            | Self::StringFromDoubleTemporary
            | Self::StringFromFloatTemporary
            | Self::StringFromBoolTemporary
            | Self::StringFromCharTemporary
            | Self::StringJoinTemporary
            | Self::StringSubstringFromTemporary
            | Self::StringSubstringRangeTemporary
            | Self::ConsoleReadLineTemporary
            | Self::FileReadAllTextTemporary(_) => Some(AllocationRegion::Temporary),
            Self::Log
            | Self::LogWarning
            | Self::LogError
            | Self::StringEquals
            | Self::StringLength
            | Self::StringContains
            | Self::StringStartsWith
            | Self::StringEndsWith
            | Self::StringIndexOf
            | Self::ReportRuntimeError(_)
            | Self::ListVersionMismatch
            | Self::TaskRun
            | Self::TaskWait
            | Self::AsyncSpawn
            | Self::AsyncState
            | Self::AsyncSetState
            | Self::AsyncStoreSlot
            | Self::AsyncLoadSlot
            | Self::AsyncSpawnInner
            | Self::AsyncAwaitResult
            | Self::AsyncSetResult
            | Self::ParallelFor
            | Self::ParallelForEach
            | Self::ParallelReduce
            | Self::StringTryParseBool
            | Self::StringTryParseInt
            | Self::StringTryParseUInt
            | Self::StringTryParseLong
            | Self::StringTryParseULong
            | Self::StringTryParseFloat
            | Self::StringTryParseDouble
            | Self::ConsoleWrite
            | Self::ConsoleWriteLine
            | Self::FileWriteAllText(_)
            | Self::FileListFiles(_)
            | Self::FileListFilesTemporary(_) => None,
        }
    }

    /// Region carried by every intrinsic that creates ASTER-managed dynamic
    /// storage. Unlike [`Self::string_allocation_region`], this includes
    /// aggregate-producing intrinsics such as `ListFiles`.
    #[must_use]
    pub const fn allocation_region(self) -> Option<AllocationRegion> {
        match self {
            Self::FileListFiles(_) => Some(AllocationRegion::Persistent),
            Self::FileListFilesTemporary(_) => Some(AllocationRegion::Temporary),
            _ => self.string_allocation_region(),
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
                Self::StringFromFloat | Self::StringFromFloatTemporary,
                AllocationRegion::Persistent,
            ) => Self::StringFromFloat,
            (
                Self::StringFromFloat | Self::StringFromFloatTemporary,
                AllocationRegion::Temporary,
            ) => Self::StringFromFloatTemporary,
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
            (
                Self::StringSubstringFrom | Self::StringSubstringFromTemporary,
                AllocationRegion::Persistent,
            ) => Self::StringSubstringFrom,
            (
                Self::StringSubstringFrom | Self::StringSubstringFromTemporary,
                AllocationRegion::Temporary,
            ) => Self::StringSubstringFromTemporary,
            (
                Self::StringSubstringRange | Self::StringSubstringRangeTemporary,
                AllocationRegion::Persistent,
            ) => Self::StringSubstringRange,
            (
                Self::StringSubstringRange | Self::StringSubstringRangeTemporary,
                AllocationRegion::Temporary,
            ) => Self::StringSubstringRangeTemporary,
            (
                Self::ConsoleReadLine | Self::ConsoleReadLineTemporary,
                AllocationRegion::Persistent,
            ) => Self::ConsoleReadLine,
            (
                Self::ConsoleReadLine | Self::ConsoleReadLineTemporary,
                AllocationRegion::Temporary,
            ) => Self::ConsoleReadLineTemporary,
            (
                Self::FileReadAllText(layout) | Self::FileReadAllTextTemporary(layout),
                AllocationRegion::Persistent,
            ) => Self::FileReadAllText(layout),
            (
                Self::FileReadAllText(layout) | Self::FileReadAllTextTemporary(layout),
                AllocationRegion::Temporary,
            ) => Self::FileReadAllTextTemporary(layout),
            _ => self,
        }
    }

    /// Return the equivalent dynamic-allocation intrinsic for `region`.
    #[must_use]
    pub const fn with_allocation_region(self, region: AllocationRegion) -> Self {
        match (self, region) {
            (
                Self::FileListFiles(layout) | Self::FileListFilesTemporary(layout),
                AllocationRegion::Persistent,
            ) => Self::FileListFiles(layout),
            (
                Self::FileListFiles(layout) | Self::FileListFilesTemporary(layout),
                AllocationRegion::Temporary,
            ) => Self::FileListFilesTemporary(layout),
            _ => self.with_string_allocation_region(region),
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
    /// Reads the `length` field of a `List<T>` header (see
    /// `aster_runtime::AsterList`).
    ListLength(Operand),
    DictionaryLength(Operand),
    /// Reads the current structural-modification counter from a `List<T>`
    /// header. Used only by `foreach`'s fail-fast mutation check (captured
    /// once, then compared before each element read); never exposed as an
    /// Aster-level property.
    ListVersion(Operand),
    /// Reads a `string`'s UTF-8 payload length in bytes (never a scalar
    /// count -- see `string.Length`). Used only by `foreach`'s cursor
    /// lowering over `string`; never exposed as an Aster-level property.
    StringByteLength(Operand),
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

/// Deterministic structural identity for a concrete `Type`, used as
/// `List<T>`'s `element_type_key`.
///
/// Distinguishes any two concrete types that could otherwise share a byte
/// layout (e.g. two unrelated single-`int`-field classes): every nominal
/// variant folds in its own monomorphized `SymbolId`, which the compiler
/// never reuses across distinct concrete declarations. Depends only on the
/// type's own structure — never a Rust pointer, a memory address, or a
/// textual name comparison — so it stays valid across arenas and is safe to
/// embed as a plain runtime constant.
#[must_use]
pub fn type_key(type_: &Type) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn mix(hash: u64, value: u64) -> u64 {
        (hash ^ value).wrapping_mul(FNV_PRIME)
    }

    fn walk(type_: &Type, hash: u64) -> u64 {
        match type_ {
            Type::Void => mix(hash, 0),
            Type::Bool => mix(hash, 1),
            Type::SByte => mix(hash, 2),
            Type::Byte => mix(hash, 3),
            Type::Short => mix(hash, 4),
            Type::UShort => mix(hash, 5),
            Type::Int => mix(hash, 6),
            Type::UInt => mix(hash, 7),
            Type::Long => mix(hash, 8),
            Type::ULong => mix(hash, 9),
            Type::Float => mix(hash, 10),
            Type::Double => mix(hash, 11),
            Type::Decimal => mix(hash, 12),
            Type::Char => mix(hash, 13),
            Type::String => mix(hash, 14),
            Type::User(symbol) => mix(mix(hash, 15), u64::from(symbol.0)),
            Type::Class(symbol) => mix(mix(hash, 16), u64::from(symbol.0)),
            Type::Interface(symbol) => mix(mix(hash, 17), u64::from(symbol.0)),
            Type::Enum(symbol) => mix(mix(hash, 18), u64::from(symbol.0)),
            Type::Array(element) => walk(element, mix(hash, 19)),
            Type::Task(result) => walk(result, mix(hash, 20)),
            Type::List(element) => walk(element, mix(hash, 21)),
            Type::Dictionary(key, value) => walk(value, walk(key, mix(hash, 22))),
            Type::Unknown => mix(hash, 22),
        }
    }

    walk(type_, FNV_OFFSET)
}

#[cfg(test)]
mod type_key_tests {
    use super::{SymbolId, Type, type_key};

    #[test]
    fn identical_scalar_types_share_a_key() {
        assert_eq!(type_key(&Type::Int), type_key(&Type::Int));
    }

    #[test]
    fn distinct_scalar_types_never_collide() {
        assert_ne!(type_key(&Type::Int), type_key(&Type::Long));
        assert_ne!(type_key(&Type::Int), type_key(&Type::UInt));
    }

    #[test]
    fn list_of_int_differs_from_a_bare_int_and_from_an_array_of_int() {
        let list_int = Type::List(Box::new(Type::Int));
        assert_ne!(type_key(&list_int), type_key(&Type::Int));
        assert_ne!(
            type_key(&list_int),
            type_key(&Type::Array(Box::new(Type::Int)))
        );
    }

    #[test]
    fn nested_lists_are_distinguished_by_depth() {
        let list_int = Type::List(Box::new(Type::Int));
        let list_list_int = Type::List(Box::new(list_int.clone()));
        assert_ne!(type_key(&list_int), type_key(&list_list_int));
    }

    #[test]
    fn same_class_symbol_produces_the_same_key_for_list_of_that_class() {
        let a = Type::List(Box::new(Type::Class(SymbolId(7))));
        let b = Type::List(Box::new(Type::Class(SymbolId(7))));
        assert_eq!(type_key(&a), type_key(&b));
    }

    #[test]
    fn two_classes_with_identical_layout_but_different_symbols_never_collide() {
        let a = Type::List(Box::new(Type::Class(SymbolId(1))));
        let b = Type::List(Box::new(Type::Class(SymbolId(2))));
        assert_ne!(type_key(&a), type_key(&b));
    }

    #[test]
    fn list_of_class_differs_from_list_of_interface_with_the_same_symbol_id() {
        let class = Type::List(Box::new(Type::Class(SymbolId(3))));
        let interface = Type::List(Box::new(Type::Interface(SymbolId(3))));
        assert_ne!(type_key(&class), type_key(&interface));
    }

    #[test]
    fn key_is_stable_across_repeated_calls() {
        let type_ = Type::List(Box::new(Type::String));
        let first = type_key(&type_);
        let second = type_key(&type_);
        assert_eq!(first, second);
    }
}
