//! Per-execution ownership and the array/list runtime ABI.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::mem::{align_of, size_of};
use std::ptr;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::arena::{
    ArenaAllocError, ArenaEventMetrics, ArenaMark, ArenaRewindMetrics, ArenaTelemetrySnapshot,
    MAX_ALIGN, PagedArena,
};
#[cfg(test)]
use crate::arena::{DEFAULT_PAGE_SIZE, MIN_PAGE_SIZE};
use crate::memory_governor::{MemoryGovernor, MemoryGovernorTelemetry};
use crate::string::AsterStrHeader;

/// Maximum arena capacity reserved by one execution context, shared by its
/// persistent and temporary regions. One GiB preserves the externally
/// validated 50,000,000-element `long` array (400,000,000 payload bytes plus
/// arena overhead) with more than twice that workload's headroom, while still
/// rejecting multi-gigabyte requests before host overcommit or an allocator
/// abort can make failure process-wide.
pub const EXECUTION_ALLOCATION_LIMIT_BYTES: usize = 1024 * 1024 * 1024;

/// Maximum number of simultaneously active ASTER function calls in one
/// execution context. This is intentionally far below the native-stack depth
/// where the smaller worker stacks have been observed to abort.
pub const ASTER_CALL_DEPTH_LIMIT: u32 = 1024;

fn checked_allocation_size(count: usize, element_size: usize) -> Option<usize> {
    count.checked_mul(element_size)
}

/// Stable array header visible to generated code only through runtime calls.
#[repr(C)]
pub struct AsterArray {
    data: *mut u8,
    length: i32,
    element_size: u32,
}

/// Offsets in the private array header ABI shared with generated code.
///
/// These are target-local constants derived from the `repr(C)` header rather
/// than duplicated layout assumptions in a backend. They are not a public
/// foreign-function interface.
pub const ASTER_ARRAY_DATA_OFFSET: usize = std::mem::offset_of!(AsterArray, data);
pub const ASTER_ARRAY_LENGTH_OFFSET: usize = std::mem::offset_of!(AsterArray, length);

fn array_allocation_layout(length: usize, element_size: usize) -> Option<(usize, usize, usize)> {
    let payload_bytes = checked_allocation_size(length, element_size)?.max(element_size);
    let data_offset = checked_align_up(size_of::<AsterArray>(), 8)?;
    let allocation_bytes = data_offset.checked_add(payload_bytes)?;
    Some((payload_bytes, data_offset, allocation_bytes))
}

impl AsterArray {
    /// Element stride recorded when this runtime-owned header was allocated.
    /// Host ABI adapters use it to validate scalar transport before reading.
    #[must_use]
    pub fn element_size(&self) -> u32 {
        self.element_size
    }
}

/// Which arena owns a `List<T>`'s header and (once grown) its buffer.
/// Recorded explicitly at allocation time because a future grow operation
/// must reuse the exact same arena â€” never deduced later by comparing
/// pointer addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ListRegion {
    Persistent,
    Temporary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DictionaryKeyKind {
    Bool = 1,
    Char = 2,
    SByte = 3,
    Byte = 4,
    Short = 5,
    UShort = 6,
    Int = 7,
    UInt = 8,
    Long = 9,
    ULong = 10,
    String = 11,
}

impl DictionaryKeyKind {
    fn from_abi(value: i32) -> Option<Self> {
        Some(match value {
            1 => Self::Bool,
            2 => Self::Char,
            3 => Self::SByte,
            4 => Self::Byte,
            5 => Self::Short,
            6 => Self::UShort,
            7 => Self::Int,
            8 => Self::UInt,
            9 => Self::Long,
            10 => Self::ULong,
            11 => Self::String,
            _ => return None,
        })
    }
}

fn dictionary_key_kind_size(kind: DictionaryKeyKind) -> usize {
    match kind {
        DictionaryKeyKind::Bool | DictionaryKeyKind::SByte | DictionaryKeyKind::Byte => 1,
        DictionaryKeyKind::Short | DictionaryKeyKind::UShort => 2,
        DictionaryKeyKind::Char | DictionaryKeyKind::Int | DictionaryKeyKind::UInt => 4,
        DictionaryKeyKind::Long | DictionaryKeyKind::ULong => 8,
        DictionaryKeyKind::String => size_of::<*const AsterStrHeader>(),
    }
}

/// Runtime-owned header for one `List<T>` instance. `T`'s content never lives
/// inline in the value that names the list (that value is always a pointer
/// to this header, exactly like `Array`/`Class`); this struct owns the
/// growable buffer instead.
///
/// Invariant: `data` is only ever dereferenced when `capacity > 0`. A freshly
/// allocated list has `capacity == 0`, and `data` is `null` in that case â€”
/// an internal detail of this header, never a value observable from Aster
/// (`List<T>` itself is never `null`; only this empty-buffer sentinel is).
#[repr(C)]
pub struct AsterList {
    data: *mut u8,
    length: i32,
    capacity: i32,
    element_size: u32,
    element_align: u32,
    /// Deterministic structural identity of the concrete element type `T`
    /// (see `aster_mir::type_key`), computed once by the compiler and never
    /// recomputed or reinterpreted by this crate.
    element_type_key: u64,
    /// Structural-modification counter, incremented after every successful
    /// `Add`/`RemoveAt` (never on a failed operation, never on `Length`/
    /// `Get`). `foreach` captures this once and compares before each element
    /// read to fail fast on structural mutation during iteration -- never
    /// exposed as an Aster-level property. 64 bits wide so overflow within
    /// one valid execution is not a practical concern; a wrapping increment
    /// is safe because equality (not ordering) is all iteration ever checks.
    version: u64,
    region: ListRegion,
    /// Depth of `temporary_scopes` when this header was allocated, or 0 for
    /// Persistent lists. Buffer allocations in `grow_list_buffer` that happen
    /// inside a deeper nested scope (i.e. a helper function) are promoted to
    /// the permanent arena so they survive the helper's temp-scope rewind.
    birth_scope_depth: u32,
}

/// Native mutable UTF-8 construction header. The header and every backing
/// buffer are owned by one execution context; immutable strings never alias
/// `data` and are produced as independent snapshots.
#[repr(C)]
pub struct AsterStringBuilder {
    data: *mut u8,
    length: usize,
    capacity: usize,
    region: ListRegion,
    birth_scope_depth: u32,
}

const STRING_BUILDER_INITIAL_CAPACITY: usize = 16;

fn string_builder_growth_capacity(required: usize) -> Option<usize> {
    required
        .max(STRING_BUILDER_INITIAL_CAPACITY)
        .checked_next_power_of_two()
}

/// Native header for one concrete insertion-ordered `Dictionary<K, V>`.
/// Buckets and entries are allocated lazily in the same arena as this header.
/// Hash seeds remain per-context; snapshots need no iterator/version state.
#[repr(C)]
pub struct AsterDictionary {
    buckets: *mut u32,
    entries: *mut u8,
    length: i32,
    bucket_capacity: i32,
    entry_capacity: i32,
    entry_count: i32,
    key_size: u32,
    key_align: u32,
    value_size: u32,
    value_align: u32,
    key_type_key: u64,
    value_type_key: u64,
    owner_id: u64,
    key_kind: DictionaryKeyKind,
    region: ListRegion,
    /// Depth of `temporary_scopes` when this header was allocated, or 0 for
    /// Persistent dictionaries. Buffer allocations inside a deeper nested scope
    /// are promoted to the permanent arena so they survive the helper's rewind.
    birth_scope_depth: u32,
}

impl AsterDictionary {
    #[must_use]
    pub fn region(&self) -> ListRegion {
        self.region
    }
}

const DICTIONARY_EMPTY_BUCKET: u32 = u32::MAX;
const DICTIONARY_INITIAL_CAPACITY: i32 = 8;
const DICTIONARY_MAX_ENTRIES: i32 = 100_000;
const DICTIONARY_MAX_ACTIVE_BYTES: usize = 64 * 1024 * 1024;
const DICTIONARY_ENTRY_HEADER_SIZE: usize = 16;

#[derive(Clone, Copy)]
struct DictionaryEntryLayout {
    key_offset: usize,
    value_offset: usize,
    stride: usize,
    align: usize,
}

fn checked_align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment.checked_sub(1)?)
        .map(|value| value & !(alignment - 1))
}

fn dictionary_entry_layout(
    key_size: u32,
    key_align: u32,
    value_size: u32,
    value_align: u32,
) -> Option<DictionaryEntryLayout> {
    let key_align = usize::try_from(key_align).ok()?;
    let value_align = usize::try_from(value_align).ok()?;
    if !key_align.is_power_of_two()
        || !value_align.is_power_of_two()
        || key_align > MAX_ALIGN
        || value_align > MAX_ALIGN
    {
        return None;
    }
    let key_offset = checked_align_up(DICTIONARY_ENTRY_HEADER_SIZE, key_align)?;
    let value_offset = checked_align_up(
        key_offset.checked_add(usize::try_from(key_size).ok()?)?,
        value_align,
    )?;
    let align = 8_usize.max(key_align).max(value_align);
    let stride = checked_align_up(
        value_offset.checked_add(usize::try_from(value_size).ok()?)?,
        align,
    )?;
    Some(DictionaryEntryLayout {
        key_offset,
        value_offset,
        stride,
        align,
    })
}

fn dictionary_storage_fits(
    bucket_capacity: i32,
    entry_capacity: i32,
    layout: DictionaryEntryLayout,
) -> bool {
    usize::try_from(bucket_capacity)
        .ok()
        .and_then(|capacity| capacity.checked_mul(size_of::<u32>()))
        .zip(
            usize::try_from(entry_capacity)
                .ok()
                .and_then(|capacity| capacity.checked_mul(layout.stride)),
        )
        .and_then(|(buckets, entries)| buckets.checked_add(entries))
        .is_some_and(|bytes| bytes <= DICTIONARY_MAX_ACTIVE_BYTES)
}

#[inline]
fn sip_round(v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64) {
    *v0 = v0.wrapping_add(*v1);
    *v1 = v1.rotate_left(13);
    *v1 ^= *v0;
    *v0 = v0.rotate_left(32);
    *v2 = v2.wrapping_add(*v3);
    *v3 = v3.rotate_left(16);
    *v3 ^= *v2;
    *v0 = v0.wrapping_add(*v3);
    *v3 = v3.rotate_left(21);
    *v3 ^= *v0;
    *v2 = v2.wrapping_add(*v1);
    *v1 = v1.rotate_left(17);
    *v1 ^= *v2;
    *v2 = v2.rotate_left(32);
}

fn siphash13(k0: u64, k1: u64, bytes: &[u8]) -> u64 {
    let mut v0 = k0 ^ 0x736f_6d65_7073_6575;
    let mut v1 = k1 ^ 0x646f_7261_6e64_6f6d;
    let mut v2 = k0 ^ 0x6c79_6765_6e65_7261;
    let mut v3 = k1 ^ 0x7465_6462_7974_6573;
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let mut word = [0_u8; 8];
        word.copy_from_slice(chunk);
        let message = u64::from_le_bytes(word);
        v3 ^= message;
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
        v0 ^= message;
    }
    let mut tail = (bytes.len() as u64) << 56;
    for (index, byte) in chunks.remainder().iter().enumerate() {
        tail |= u64::from(*byte) << (index * 8);
    }
    v3 ^= tail;
    sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    v0 ^= tail;
    v2 ^= 0xff;
    for _ in 0..3 {
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    }
    v0 ^ v1 ^ v2 ^ v3
}

impl AsterList {
    #[must_use]
    pub fn length(&self) -> i32 {
        self.length
    }

    #[must_use]
    pub fn capacity(&self) -> i32 {
        self.capacity
    }

    #[must_use]
    pub fn element_size(&self) -> u32 {
        self.element_size
    }

    #[must_use]
    pub fn element_align(&self) -> u32 {
        self.element_align
    }

    #[must_use]
    pub fn element_type_key(&self) -> u64 {
        self.element_type_key
    }

    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    #[must_use]
    pub fn region(&self) -> ListRegion {
        self.region
    }
}

/// Immutable snapshot of allocation metrics for one execution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryStats {
    pub total_allocations: u64,
    pub object_allocations: u64,
    pub array_allocations: u64,
    pub string_allocations: u64,
    pub requested_bytes: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub peak_used_bytes: u64,
    pub peak_reserved_bytes: u64,
}

/// Experimental allocator event counters used by the AARM research matrix.
///
/// This surface is intentionally separate from [`MemoryStats`] and is not
/// printed by the stable `--memory-stats` CLI output.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AarmAllocatorEvents {
    pub active_page_fast_path_allocations: u64,
    pub slow_path_allocations: u64,
    pub inactive_page_reuse_events: u64,
    pub fresh_regular_page_allocations: u64,
    pub fresh_oversized_page_allocations: u64,
    pub rewind_events: u64,
    pub rewound_bytes: u64,
    pub allocation_limit_denials: u64,
}

/// State transition recorded for the most recent rewind of one arena.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AarmRewindTelemetry {
    pub live_used_bytes_before: u64,
    pub live_used_bytes_after: u64,
    pub arena_capacity_bytes_before: u64,
    pub arena_capacity_bytes_after: u64,
    pub active_page_capacity_bytes_after: u64,
    pub inactive_page_capacity_bytes_after: u64,
}

/// Experimental point-in-time and peak measurements for one arena region.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AarmRegionTelemetry {
    pub live_used_bytes: u64,
    pub arena_capacity_bytes: u64,
    pub virtual_extent_bytes: Option<u64>,
    pub backing_retained_bytes: Option<u64>,
    pub backing_discarded_bytes: Option<u64>,
    pub peak_backing_retained_bytes: Option<u64>,
    pub active_page_capacity_bytes: u64,
    pub inactive_page_capacity_bytes: u64,
    pub page_count: u64,
    pub active_page_count: u64,
    pub inactive_page_count: u64,
    pub peak_live_used_bytes: u64,
    pub peak_arena_capacity_bytes: u64,
    pub events: AarmAllocatorEvents,
    pub last_rewind: Option<AarmRewindTelemetry>,
}

/// Opt-in AARM measurement snapshot for one execution context.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AarmMemoryTelemetry {
    pub requested_bytes: u64,
    pub temporary: AarmRegionTelemetry,
    pub persistent: AarmRegionTelemetry,
    pub total: AarmRegionTelemetry,
    pub governor: Option<MemoryGovernorTelemetry>,
}

impl From<ArenaEventMetrics> for AarmAllocatorEvents {
    fn from(events: ArenaEventMetrics) -> Self {
        Self {
            active_page_fast_path_allocations: events.active_page_fast_path_allocations,
            slow_path_allocations: events.slow_path_allocations,
            inactive_page_reuse_events: events.inactive_page_reuse_events,
            fresh_regular_page_allocations: events.fresh_regular_page_allocations,
            fresh_oversized_page_allocations: events.fresh_oversized_page_allocations,
            rewind_events: events.rewind_events,
            rewound_bytes: events.rewound_bytes,
            allocation_limit_denials: events.allocation_limit_denials,
        }
    }
}

impl From<ArenaRewindMetrics> for AarmRewindTelemetry {
    fn from(rewind: ArenaRewindMetrics) -> Self {
        Self {
            live_used_bytes_before: rewind.used_bytes_before as u64,
            live_used_bytes_after: rewind.used_bytes_after as u64,
            arena_capacity_bytes_before: rewind.capacity_bytes_before as u64,
            arena_capacity_bytes_after: rewind.capacity_bytes_after as u64,
            active_page_capacity_bytes_after: rewind.active_page_capacity_bytes_after as u64,
            inactive_page_capacity_bytes_after: rewind.inactive_page_capacity_bytes_after as u64,
        }
    }
}

impl From<ArenaTelemetrySnapshot> for AarmRegionTelemetry {
    fn from(snapshot: ArenaTelemetrySnapshot) -> Self {
        Self {
            live_used_bytes: snapshot.used_bytes as u64,
            arena_capacity_bytes: snapshot.capacity_bytes as u64,
            virtual_extent_bytes: snapshot.virtual_extent_bytes.map(|value| value as u64),
            backing_retained_bytes: snapshot.backing_retained_bytes.map(|value| value as u64),
            backing_discarded_bytes: snapshot.backing_discarded_bytes.map(|value| value as u64),
            peak_backing_retained_bytes: snapshot
                .peak_backing_retained_bytes
                .map(|value| value as u64),
            active_page_capacity_bytes: snapshot.active_page_capacity_bytes as u64,
            inactive_page_capacity_bytes: snapshot.inactive_page_capacity_bytes as u64,
            page_count: snapshot.page_count as u64,
            active_page_count: snapshot.active_page_count as u64,
            inactive_page_count: snapshot.inactive_page_count as u64,
            peak_live_used_bytes: snapshot.peak_used_bytes as u64,
            peak_arena_capacity_bytes: snapshot.peak_capacity_bytes as u64,
            events: snapshot.events.into(),
            last_rewind: snapshot.last_rewind.map(Into::into),
        }
    }
}

impl AarmAllocatorEvents {
    fn sum(left: Self, right: Self) -> Self {
        Self {
            active_page_fast_path_allocations: left.active_page_fast_path_allocations
                + right.active_page_fast_path_allocations,
            slow_path_allocations: left.slow_path_allocations + right.slow_path_allocations,
            inactive_page_reuse_events: left.inactive_page_reuse_events
                + right.inactive_page_reuse_events,
            fresh_regular_page_allocations: left.fresh_regular_page_allocations
                + right.fresh_regular_page_allocations,
            fresh_oversized_page_allocations: left.fresh_oversized_page_allocations
                + right.fresh_oversized_page_allocations,
            rewind_events: left.rewind_events + right.rewind_events,
            rewound_bytes: left.rewound_bytes + right.rewound_bytes,
            allocation_limit_denials: left.allocation_limit_denials
                + right.allocation_limit_denials,
        }
    }
}

#[derive(Clone, Copy)]
enum AllocationCategory {
    Object,
    Array,
    String,
}

#[derive(Clone, Copy)]
enum AllocationLimitKind {
    Execution,
    TaskDomainMain,
    TaskEntitlement,
    AsyncDomainMain,
    AsyncMoveNextEntitlement,
    AsyncInnerEntitlement,
}

/// Opaque checkpoint for the temporary arena owned by one execution context.
///
/// The token is intentionally neither `Copy` nor `Clone`: rewinding consumes
/// it, so the same checkpoint cannot be applied twice through this API.
#[must_use = "temporary arena marks must be rewound in LIFO order"]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct TemporaryArenaMark(ArenaMark);

/// Owns every dynamic allocation made by one JIT invocation.
/// Persistent allocations live in `arena`; function-local objects, arrays,
/// and strings can use `temporary_arena`. Both arenas are released in bulk
/// when the context is dropped, while temporary scopes rewind on return.
pub struct ExecutionContext {
    arena: PagedArena,
    temporary_arena: PagedArena,
    /// Compiler-authorized fine-grained checkpoint nested inside the current
    /// semantic function Temporary scope. This is deliberately separate from
    /// `temporary_scopes`: collection and builder promotion uses that vector's
    /// length as semantic function-scope depth, and a fine checkpoint must not
    /// change it.
    fine_temporary_subregion: Option<TemporaryArenaMark>,
    temporary_scopes: Vec<TemporaryArenaMark>,
    allocation_limit_bytes: usize,
    allocation_limit_kind: AllocationLimitKind,
    call_depth: u32,
    error: Option<String>,
    collect_stats: bool,
    stats: MemoryStats,
    /// Opaque host extension slot: an untyped handle to whatever the current
    /// top-level execution registered (e.g. a task-execution pool), set by
    /// the host before invoking the entry function. This crate never reads
    /// or writes through the pointer itself; only the host that set it knows
    /// its real type. Absent (`None`) for every sequential invocation that
    /// does not opt in.
    task_runtime: Option<*mut ()>,
    /// Console I/O backend for `aster.io.Write`/`WriteLine`/`ReadLine`.
    /// Owned per-context (never a global or singleton), so independent
    /// contexts never share output or input. Lazily defaults to real
    /// stdin/stdout on first use; a host (tests, or a future CLI override)
    /// can inject an in-memory backend first via [`Self::set_console_backend`].
    console: Option<Box<dyn crate::io::ConsoleBackend>>,
    /// Filesystem backend for `aster.io.ReadAllText`/`WriteAllText`. Owned
    /// per-context, never a global/singleton/registry, and never shared
    /// automatically with workers. Lazily defaults to the real filesystem on
    /// first use; a host (tests, or a future CLI override) can inject an
    /// in-memory backend first via [`Self::set_filesystem_backend`].
    filesystem: Option<Box<dyn crate::filesystem::FileSystemBackend>>,
    dictionary_hash_k0: u64,
    dictionary_hash_k1: u64,
    dictionary_owner_id: u64,
}

static NEXT_DICTIONARY_OWNER_ID: AtomicU64 = AtomicU64::new(1);

fn dictionary_context_identity() -> (u64, u64, u64) {
    let state = RandomState::new();
    let mut first = state.build_hasher();
    first.write_u64(0x6173_7465_722d_6b30);
    let mut second = state.build_hasher();
    second.write_u64(0x6173_7465_722d_6b31);
    let owner = NEXT_DICTIONARY_OWNER_ID.fetch_add(1, Ordering::Relaxed);
    (first.finish(), second.finish(), owner)
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionContext {
    /// Existing local safety ceiling used by experimental AARM planners.
    #[doc(hidden)]
    pub const AARM_MAX_CONTEXT_CAPACITY_BYTES: usize = EXECUTION_ALLOCATION_LIMIT_BYTES;

    /// Smallest fresh arena page capacity used by experimental AARM planners.
    #[doc(hidden)]
    pub const AARM_MIN_PAGE_CAPACITY_BYTES: usize = crate::arena::MIN_PAGE_SIZE;

    /// Largest regular arena page capacity used by experimental AARM tests.
    #[doc(hidden)]
    pub const AARM_DEFAULT_PAGE_CAPACITY_BYTES: usize = crate::arena::DEFAULT_PAGE_SIZE;

    #[must_use]
    pub fn new() -> Self {
        Self::with_options(false, EXECUTION_ALLOCATION_LIMIT_BYTES)
    }

    fn with_options(collect_stats: bool, allocation_limit_bytes: usize) -> Self {
        Self::with_options_and_governor(collect_stats, allocation_limit_bytes, None)
    }

    fn with_options_and_governor(
        collect_stats: bool,
        allocation_limit_bytes: usize,
        governor: Option<Arc<MemoryGovernor>>,
    ) -> Self {
        let (dictionary_hash_k0, dictionary_hash_k1, dictionary_owner_id) =
            dictionary_context_identity();
        let (arena, temporary_arena) = governor.map_or_else(
            || (PagedArena::new(), PagedArena::new()),
            |governor| {
                (
                    PagedArena::with_memory_governor(Arc::clone(&governor)),
                    PagedArena::with_memory_governor(governor),
                )
            },
        );
        Self {
            arena,
            temporary_arena,
            fine_temporary_subregion: None,
            temporary_scopes: Vec::new(),
            allocation_limit_bytes,
            allocation_limit_kind: AllocationLimitKind::Execution,
            call_depth: 0,
            error: None,
            collect_stats,
            stats: MemoryStats::default(),
            task_runtime: None,
            console: None,
            filesystem: None,
            dictionary_hash_k0,
            dictionary_hash_k1,
            dictionary_owner_id,
        }
    }

    #[must_use]
    pub fn with_stats() -> Self {
        Self::with_options(true, EXECUTION_ALLOCATION_LIMIT_BYTES)
    }

    /// Create an experimental statistics-enabled context governed by one
    /// explicit shared hard budget.
    #[doc(hidden)]
    #[must_use]
    pub fn with_memory_governor(governor: Arc<MemoryGovernor>) -> Self {
        Self::with_memory_budget(governor, EXECUTION_ALLOCATION_LIMIT_BYTES)
    }

    /// Create an experimental statistics-enabled context with both a local
    /// retained-capacity ceiling and one shared hard budget.
    #[doc(hidden)]
    #[must_use]
    pub fn with_memory_budget(
        governor: Arc<MemoryGovernor>,
        allocation_limit_bytes: usize,
    ) -> Self {
        Self::with_options_and_governor(
            true,
            allocation_limit_bytes.min(EXECUTION_ALLOCATION_LIMIT_BYTES),
            Some(governor),
        )
    }

    /// Create one experimental governed plain-task context. The byte ceiling
    /// remains local to this invocation and actual fresh pages still require
    /// admission from the shared governor.
    #[doc(hidden)]
    #[must_use]
    pub fn with_task_memory_budget(
        governor: Arc<MemoryGovernor>,
        allocation_limit_bytes: usize,
    ) -> Self {
        let mut context = Self::with_memory_budget(governor, allocation_limit_bytes);
        context.allocation_limit_kind = AllocationLimitKind::TaskEntitlement;
        context
    }

    /// Create one experimental governed async `MoveNext` context.
    #[doc(hidden)]
    #[must_use]
    pub fn with_async_move_next_memory_budget(
        governor: Arc<MemoryGovernor>,
        allocation_limit_bytes: usize,
    ) -> Self {
        let mut context = Self::with_memory_budget(governor, allocation_limit_bytes);
        context.allocation_limit_kind = AllocationLimitKind::AsyncMoveNextEntitlement;
        context
    }

    /// Create one experimental governed awaited-inner context.
    #[doc(hidden)]
    #[must_use]
    pub fn with_async_inner_memory_budget(
        governor: Arc<MemoryGovernor>,
        allocation_limit_bytes: usize,
    ) -> Self {
        let mut context = Self::with_memory_budget(governor, allocation_limit_bytes);
        context.allocation_limit_kind = AllocationLimitKind::AsyncInnerEntitlement;
        context
    }

    /// Freeze the governed main context at its already-retained capacity plus
    /// a deterministic future-growth entitlement. Returns the effective local
    /// ceiling, or `None` if the arithmetic is not addressable.
    #[doc(hidden)]
    pub fn freeze_task_domain_main_limit(&mut self, future_growth_bytes: usize) -> Option<usize> {
        let retained = self.retained_arena_capacity_bytes()?;
        let requested = retained.checked_add(future_growth_bytes)?;
        self.allocation_limit_bytes = self.allocation_limit_bytes.min(requested).max(retained);
        self.allocation_limit_kind = AllocationLimitKind::TaskDomainMain;
        Some(self.allocation_limit_bytes)
    }

    /// Freeze Main at retained capacity plus its async-domain future-growth
    /// entitlement without invalidating pages it already owns.
    #[doc(hidden)]
    pub fn freeze_async_domain_main_limit(&mut self, future_growth_bytes: usize) -> Option<usize> {
        let retained = self.retained_arena_capacity_bytes()?;
        let requested = retained.checked_add(future_growth_bytes)?;
        self.allocation_limit_bytes = self.allocation_limit_bytes.min(requested).max(retained);
        self.allocation_limit_kind = AllocationLimitKind::AsyncDomainMain;
        Some(self.allocation_limit_bytes)
    }

    /// Capacity currently retained by both arenas in this context.
    #[doc(hidden)]
    #[must_use]
    pub fn retained_arena_capacity_bytes(&self) -> Option<usize> {
        self.arena
            .metrics()
            .reserved_bytes
            .checked_add(self.temporary_arena.metrics().reserved_bytes)
    }

    /// Whether both regions draw fresh-page capacity from this exact shared
    /// governor. Used to validate experimental execution-domain wiring.
    #[doc(hidden)]
    #[must_use]
    pub fn is_governed_by(&self, governor: &Arc<MemoryGovernor>) -> bool {
        self.arena.is_governed_by(governor) && self.temporary_arena.is_governed_by(governor)
    }

    #[cfg(test)]
    fn with_allocation_limit(allocation_limit_bytes: usize) -> Self {
        Self::with_options(false, allocation_limit_bytes)
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    #[must_use]
    pub fn memory_stats(&self) -> &MemoryStats {
        &self.stats
    }

    /// Return the experimental AARM allocator snapshot when this context was
    /// created in statistics mode.
    #[doc(hidden)]
    #[must_use]
    pub fn aarm_memory_telemetry(&self) -> Option<AarmMemoryTelemetry> {
        if !self.collect_stats {
            return None;
        }
        let persistent: AarmRegionTelemetry = self.arena.telemetry_snapshot()?.into();
        let temporary: AarmRegionTelemetry = self.temporary_arena.telemetry_snapshot()?.into();
        let total = AarmRegionTelemetry {
            live_used_bytes: temporary.live_used_bytes + persistent.live_used_bytes,
            arena_capacity_bytes: temporary.arena_capacity_bytes + persistent.arena_capacity_bytes,
            virtual_extent_bytes: sum_known_u64(
                temporary.virtual_extent_bytes,
                persistent.virtual_extent_bytes,
            ),
            backing_retained_bytes: sum_known_u64(
                temporary.backing_retained_bytes,
                persistent.backing_retained_bytes,
            ),
            backing_discarded_bytes: sum_known_u64(
                temporary.backing_discarded_bytes,
                persistent.backing_discarded_bytes,
            ),
            peak_backing_retained_bytes: sum_known_u64(
                temporary.peak_backing_retained_bytes,
                persistent.peak_backing_retained_bytes,
            ),
            active_page_capacity_bytes: temporary.active_page_capacity_bytes
                + persistent.active_page_capacity_bytes,
            inactive_page_capacity_bytes: temporary.inactive_page_capacity_bytes
                + persistent.inactive_page_capacity_bytes,
            page_count: temporary.page_count + persistent.page_count,
            active_page_count: temporary.active_page_count + persistent.active_page_count,
            inactive_page_count: temporary.inactive_page_count + persistent.inactive_page_count,
            peak_live_used_bytes: self.stats.peak_used_bytes,
            peak_arena_capacity_bytes: self.stats.peak_reserved_bytes,
            events: AarmAllocatorEvents::sum(temporary.events, persistent.events),
            last_rewind: temporary.last_rewind.or(persistent.last_rewind),
        };
        Some(AarmMemoryTelemetry {
            requested_bytes: self.stats.requested_bytes,
            temporary,
            persistent,
            total,
            governor: self.arena.governor_telemetry(),
        })
    }

    /// Record a controlled runtime error. First-error-wins: later calls are
    /// ignored once an error is already recorded. Public so host-provided
    /// ABI functions defined outside this crate (e.g. a JIT backend's task
    /// support) can report failures through the same channel as every
    /// built-in runtime function.
    pub fn fail(&mut self, message: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(message.into());
        }
    }

    fn enter_call(&mut self) -> bool {
        if self.call_depth >= ASTER_CALL_DEPTH_LIMIT {
            self.fail(format!(
                "ASTER call depth exceeds the supported limit of {ASTER_CALL_DEPTH_LIMIT}"
            ));
            return false;
        }
        self.call_depth += 1;
        true
    }

    fn leave_call(&mut self) {
        let Some(depth) = self.call_depth.checked_sub(1) else {
            self.fail("ASTER call-depth guard is unbalanced");
            return;
        };
        self.call_depth = depth;
    }

    /// Register the current top-level execution's opaque host extension
    /// (e.g. a task-execution pool). Overwrites any previous value.
    pub fn set_task_runtime(&mut self, pointer: *mut ()) {
        self.task_runtime = Some(pointer);
    }

    /// The opaque host extension registered by [`Self::set_task_runtime`],
    /// or `None` if this execution never opted in.
    #[must_use]
    pub fn task_runtime(&self) -> Option<*mut ()> {
        self.task_runtime
    }

    /// Inject the console I/O backend this context uses for `aster.io.Write`/
    /// `WriteLine`/`ReadLine`. Overwrites any previous backend (including the
    /// lazily created default). Independent contexts never share a backend;
    /// this is per-context state, not a global or singleton.
    pub fn set_console_backend(&mut self, backend: Box<dyn crate::io::ConsoleBackend>) {
        self.console = Some(backend);
    }

    /// The active console backend, lazily defaulting to real stdin/stdout on
    /// first use so a host that never calls `set_console_backend` still gets
    /// working I/O.
    pub(crate) fn console_backend(&mut self) -> &mut dyn crate::io::ConsoleBackend {
        if self.console.is_none() {
            self.console = Some(Box::new(crate::io::StdConsoleBackend::default()));
        }
        self.console.as_deref_mut().expect("just initialized above")
    }

    /// Inject the filesystem backend this context uses for `aster.io.
    /// ReadAllText`/`WriteAllText`. Overwrites any previous backend
    /// (including the lazily created default). Independent contexts never
    /// share a backend; this is per-context state, not a global or
    /// singleton, and is never propagated to workers automatically.
    pub fn set_filesystem_backend(
        &mut self,
        backend: Box<dyn crate::filesystem::FileSystemBackend>,
    ) {
        self.filesystem = Some(backend);
    }

    /// The active filesystem backend, lazily defaulting to the real
    /// filesystem on first use so a host that never calls
    /// `set_filesystem_backend` still gets working file I/O.
    pub(crate) fn filesystem_backend(&mut self) -> &mut dyn crate::filesystem::FileSystemBackend {
        if self.filesystem.is_none() {
            self.filesystem = Some(Box::new(crate::filesystem::StdFileSystemBackend::default()));
        }
        self.filesystem
            .as_deref_mut()
            .expect("just initialized above")
    }

    fn record_allocation(&mut self, category: AllocationCategory, requested: usize) {
        if !self.collect_stats {
            return;
        }
        self.stats.total_allocations += 1;
        match category {
            AllocationCategory::Object => self.stats.object_allocations += 1,
            AllocationCategory::Array => self.stats.array_allocations += 1,
            AllocationCategory::String => self.stats.string_allocations += 1,
        }
        self.stats.requested_bytes += requested as u64;
        self.refresh_memory_usage();
    }

    fn refresh_memory_usage(&mut self) {
        if !self.collect_stats {
            return;
        }

        let persistent = self.arena.metrics();
        let temporary = self.temporary_arena.metrics();
        let used_bytes = persistent
            .used_bytes
            .checked_add(temporary.used_bytes)
            .expect("combined arena used bytes overflow");
        let reserved_bytes = persistent
            .reserved_bytes
            .checked_add(temporary.reserved_bytes)
            .expect("combined arena reserved bytes overflow");

        self.stats.used_bytes = used_bytes as u64;
        self.stats.reserved_bytes = reserved_bytes as u64;
        self.stats.peak_used_bytes = self.stats.peak_used_bytes.max(self.stats.used_bytes);
        self.stats.peak_reserved_bytes = self
            .stats
            .peak_reserved_bytes
            .max(self.stats.reserved_bytes);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn mark_temporary(&mut self) -> TemporaryArenaMark {
        TemporaryArenaMark(self.temporary_arena.mark())
    }

    fn allocate_in_region(
        &mut self,
        size: usize,
        align: usize,
        temporary: bool,
    ) -> Option<*mut u8> {
        let other_reserved = if temporary {
            self.arena.metrics().reserved_bytes
        } else {
            self.temporary_arena.metrics().reserved_bytes
        };
        let arena_limit = self.allocation_limit_bytes.saturating_sub(other_reserved);
        let result = if temporary {
            self.temporary_arena.try_alloc(size, align, arena_limit)
        } else {
            self.arena.try_alloc(size, align, arena_limit)
        };
        match result {
            Ok(pointer) => {
                self.refresh_memory_usage();
                Some(pointer)
            }
            Err(error) => {
                self.fail_allocation(error);
                None
            }
        }
    }

    fn fail_allocation(&mut self, error: ArenaAllocError) {
        match error {
            ArenaAllocError::AddressSpace => {
                self.fail("allocation size exceeds the addressable range");
            }
            ArenaAllocError::Limit => {
                let authority = match self.allocation_limit_kind {
                    AllocationLimitKind::Execution => "execution memory limit",
                    AllocationLimitKind::TaskDomainMain => {
                        "deterministic Task.Run main-context memory entitlement"
                    }
                    AllocationLimitKind::TaskEntitlement => {
                        "deterministic Task.Run memory entitlement"
                    }
                    AllocationLimitKind::AsyncDomainMain => {
                        "deterministic async Main memory entitlement"
                    }
                    AllocationLimitKind::AsyncMoveNextEntitlement => {
                        "deterministic async MoveNext memory entitlement"
                    }
                    AllocationLimitKind::AsyncInnerEntitlement => {
                        "deterministic async awaited-inner memory entitlement"
                    }
                };
                self.fail(format!(
                    "allocation exceeds the {authority} of {} bytes",
                    self.allocation_limit_bytes
                ));
            }
            ArenaAllocError::SharedLimit { hard_limit_bytes } => {
                self.fail(format!(
                    "allocation exceeds the shared execution memory budget of {hard_limit_bytes} bytes"
                ));
            }
            ArenaAllocError::OutOfMemory => {
                self.fail("the host could not satisfy an ASTER allocation request");
            }
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn allocate_temporary(&mut self, size: usize, align: usize) -> *mut u8 {
        self.allocate_in_region(size, align, true)
            .unwrap_or(ptr::null_mut())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn rewind_temporary(&mut self, mark: TemporaryArenaMark) {
        self.temporary_arena.rewind(mark.0);
        self.refresh_memory_usage();
    }

    fn enter_temporary_scope(&mut self) {
        if self.fine_temporary_subregion.is_some() {
            self.fail("temporary scope enter cannot occur while a temporary subregion is active");
            return;
        }
        let mark = self.mark_temporary();
        self.temporary_scopes.push(mark);
    }

    fn leave_temporary_scope(&mut self) {
        if self.fine_temporary_subregion.is_some() {
            self.fail("temporary scope leave cannot occur while a temporary subregion is active");
            return;
        }
        let Some(mark) = self.temporary_scopes.pop() else {
            self.fail("temporary scope leave has no matching enter");
            return;
        };
        self.rewind_temporary(mark);
    }

    fn enter_temporary_subregion(&mut self) {
        // A prior controlled failure will take generated code to its existing
        // function cleanup. Do not create a fine mark that cleanup would then
        // have to discover dynamically.
        if self.error.is_some() {
            return;
        }
        if self.temporary_scopes.is_empty() {
            self.fail("temporary subregion enter requires an active temporary scope");
            return;
        }
        if self.fine_temporary_subregion.is_some() {
            self.fail("temporary subregion enter has an active subregion");
            return;
        }

        self.fine_temporary_subregion = Some(self.mark_temporary());
    }

    fn leave_temporary_subregion(&mut self) {
        let Some(mark) = self.fine_temporary_subregion.take() else {
            self.fail("temporary subregion exit has no matching enter");
            return;
        };

        // Cleanup must run even when an allocation inside the subregion has
        // already recorded the first runtime error.
        self.rewind_temporary(mark);
    }

    pub(crate) fn allocate_temporary_object(&mut self, size: u32) -> *mut u8 {
        if self.temporary_scopes.is_empty() {
            self.fail("temporary object allocation requires an active temporary scope");
            return ptr::null_mut();
        }
        let bytes = usize::try_from(size.max(1)).unwrap_or(1);
        match self.temporary_arena.try_alloc_existing(bytes, 8) {
            Ok(Some(pointer)) => {
                self.refresh_memory_usage();
                self.record_allocation(AllocationCategory::Object, bytes);
                return pointer;
            }
            Ok(None) => {}
            Err(error) => {
                self.fail_allocation(error);
                return ptr::null_mut();
            }
        }
        let Some(pointer) = self.allocate_in_region(bytes, 8, true) else {
            return ptr::null_mut();
        };
        self.record_allocation(AllocationCategory::Object, bytes);
        pointer
    }

    fn allocate_array_in_region(
        &mut self,
        length: i32,
        element_size: u32,
        temporary: bool,
    ) -> *mut AsterArray {
        if temporary && self.temporary_scopes.is_empty() {
            self.fail("temporary array allocation requires an active temporary scope");
            return ptr::null_mut();
        }

        let valid_length = length.max(0);
        let valid_size = element_size.max(1);
        if length < 0 {
            self.fail(format!("array length cannot be negative: {length}"));
        }
        let layout = usize::try_from(valid_length)
            .ok()
            .and_then(|length| array_allocation_layout(length, valid_size as usize));
        let Some((bytes, data_offset, allocation_bytes)) = layout else {
            self.fail("array allocation size exceeds the addressable range");
            return ptr::null_mut();
        };

        let allocation_align = align_of::<AsterArray>().max(8);
        let Some(header) = self.allocate_in_region(allocation_bytes, allocation_align, temporary)
        else {
            return ptr::null_mut();
        };
        let data = header.wrapping_add(data_offset);
        #[allow(clippy::cast_ptr_alignment)]
        let header_ptr = header.cast::<AsterArray>();

        // SAFETY: `header_ptr` points to zeroed, correctly aligned memory for
        // `AsterArray`. Header and data are one arena allocation and therefore
        // have exactly the same lifetime.
        #[allow(unsafe_code)]
        unsafe {
            (*header_ptr).data = data;
            (*header_ptr).length = valid_length;
            (*header_ptr).element_size = valid_size;
        }
        self.record_allocation(AllocationCategory::Array, bytes);
        header_ptr
    }

    pub(crate) fn allocate_array(&mut self, length: i32, element_size: u32) -> *mut AsterArray {
        self.allocate_array_in_region(length, element_size, false)
    }

    pub(crate) fn allocate_temporary_array(
        &mut self,
        length: i32,
        element_size: u32,
    ) -> *mut AsterArray {
        self.allocate_array_in_region(length, element_size, true)
    }

    /// Allocate an empty `List<T>` header (`length == capacity == 0`, no
    /// buffer yet) in the arena selected by `region`. Every failure is
    /// reported through `self.fail` and returns a null pointer â€” never a
    /// panic, never a trap, never a partially written header (nothing is
    /// written to the header until every validation has passed). The empty
    /// list reserves no element capacity; the first `Add` grows it.
    pub(crate) fn allocate_list_in_region(
        &mut self,
        element_size: u32,
        element_align: u32,
        element_type_key: u64,
        region: ListRegion,
    ) -> *mut AsterList {
        if element_size == 0 {
            self.fail("list element size must be greater than zero");
            return ptr::null_mut();
        }
        if !element_align.is_power_of_two() {
            self.fail(format!(
                "list element alignment must be a nonzero power of two, got {element_align}"
            ));
            return ptr::null_mut();
        }
        if element_align as usize > MAX_ALIGN {
            self.fail(format!(
                "list element alignment {element_align} exceeds the arena's maximum supported alignment of {MAX_ALIGN}"
            ));
            return ptr::null_mut();
        }
        if region == ListRegion::Temporary && self.temporary_scopes.is_empty() {
            self.fail("temporary list allocation requires an active temporary scope");
            return ptr::null_mut();
        }

        let Some(header) = self.allocate_in_region(
            size_of::<AsterList>(),
            align_of::<AsterList>(),
            region == ListRegion::Temporary,
        ) else {
            return ptr::null_mut();
        };
        #[allow(clippy::cast_ptr_alignment)]
        let header_ptr = header.cast::<AsterList>();

        // SAFETY: `header_ptr` points to zeroed, correctly aligned memory for
        // `AsterList`, just allocated above and not yet observed anywhere
        // else; every field is written before this pointer is returned.
        #[allow(unsafe_code)]
        unsafe {
            (*header_ptr).data = ptr::null_mut();
            (*header_ptr).length = 0;
            (*header_ptr).capacity = 0;
            (*header_ptr).element_size = element_size;
            (*header_ptr).element_align = element_align;
            (*header_ptr).element_type_key = element_type_key;
            (*header_ptr).version = 0;
            (*header_ptr).region = region;
            (*header_ptr).birth_scope_depth = if region == ListRegion::Temporary {
                u32::try_from(self.temporary_scopes.len()).unwrap_or(u32::MAX)
            } else {
                0
            };
        }
        self.record_allocation(AllocationCategory::Object, size_of::<AsterList>());
        header_ptr
    }

    pub(crate) fn allocate_string_builder_in_region(
        &mut self,
        region: ListRegion,
    ) -> *mut AsterStringBuilder {
        if region == ListRegion::Temporary && self.temporary_scopes.is_empty() {
            self.fail("temporary StringBuilder allocation requires an active temporary scope");
            return ptr::null_mut();
        }
        let Some(header) = self.allocate_in_region(
            size_of::<AsterStringBuilder>(),
            align_of::<AsterStringBuilder>(),
            region == ListRegion::Temporary,
        ) else {
            return ptr::null_mut();
        };
        #[allow(clippy::cast_ptr_alignment)]
        let header = header.cast::<AsterStringBuilder>();
        // SAFETY: fresh, aligned arena memory is unpublished until every
        // field has been initialized.
        #[allow(unsafe_code)]
        unsafe {
            *header = AsterStringBuilder {
                data: ptr::null_mut(),
                length: 0,
                capacity: 0,
                region,
                birth_scope_depth: if region == ListRegion::Temporary {
                    u32::try_from(self.temporary_scopes.len()).unwrap_or(u32::MAX)
                } else {
                    0
                },
            };
        }
        self.record_allocation(AllocationCategory::Object, size_of::<AsterStringBuilder>());
        header
    }

    fn validate_string_builder(&mut self, builder: *const AsterStringBuilder) -> bool {
        if builder.is_null() {
            self.fail("StringBuilder received a null builder");
            return false;
        }
        // SAFETY: generated code can obtain this pointer only from the
        // allocation functions below and keeps its execution context alive.
        #[allow(unsafe_code)]
        let (data, length, capacity) =
            unsafe { ((*builder).data, (*builder).length, (*builder).capacity) };
        if length > capacity || (capacity == 0) != data.is_null() {
            self.fail("StringBuilder header is invalid");
            return false;
        }
        true
    }

    fn string_builder_append(
        &mut self,
        builder: *mut AsterStringBuilder,
        value: *const AsterStrHeader,
    ) {
        if self.error.is_some() || !self.validate_string_builder(builder) {
            return;
        }
        // SAFETY: `value` is an immutable ASTER string owned by this context
        // or the live JIT module and is borrowed only for this call.
        #[allow(unsafe_code)]
        let Some(value) = (unsafe { crate::string::view(value) }) else {
            self.fail("StringBuilder.Append received an invalid UTF-8 string reference");
            return;
        };
        // SAFETY: validated above.
        #[allow(unsafe_code)]
        let (old_data, old_length, old_capacity, region, birth_scope_depth) = unsafe {
            (
                (*builder).data,
                (*builder).length,
                (*builder).capacity,
                (*builder).region,
                (*builder).birth_scope_depth,
            )
        };
        let Some(required) = old_length.checked_add(value.len()) else {
            self.fail("StringBuilder length overflow while appending");
            return;
        };
        if required <= old_capacity {
            if !value.is_empty() {
                // SAFETY: `required <= old_capacity`; input is immutable and
                // cannot alias the builder's unpublished mutable storage.
                #[allow(unsafe_code)]
                unsafe {
                    ptr::copy_nonoverlapping(value.as_ptr(), old_data.add(old_length), value.len());
                    (*builder).length = required;
                }
            }
            return;
        }
        let Some(new_capacity) = string_builder_growth_capacity(required) else {
            self.fail("StringBuilder capacity overflow while growing");
            return;
        };
        if region == ListRegion::Temporary && self.temporary_scopes.is_empty() {
            self.fail("temporary StringBuilder growth requires an active temporary scope");
            return;
        }
        let use_temporary = region == ListRegion::Temporary
            && self.temporary_scopes.len() == birth_scope_depth as usize;
        let Some(new_data) = self.allocate_in_region(new_capacity, 1, use_temporary) else {
            return;
        };
        // SAFETY: the fresh buffer has `new_capacity >= required` bytes.
        // Nothing is published until both copies complete.
        #[allow(unsafe_code)]
        unsafe {
            if old_length != 0 {
                ptr::copy_nonoverlapping(old_data, new_data, old_length);
            }
            if !value.is_empty() {
                ptr::copy_nonoverlapping(value.as_ptr(), new_data.add(old_length), value.len());
            }
        }
        self.record_allocation(AllocationCategory::Object, new_capacity);
        // SAFETY: allocation and copying succeeded; publishing all three
        // fields together leaves no partially grown observable header.
        #[allow(unsafe_code)]
        unsafe {
            (*builder).data = new_data;
            (*builder).length = required;
            (*builder).capacity = new_capacity;
        }
    }

    fn string_builder_to_string(
        &mut self,
        builder: *const AsterStringBuilder,
        temporary: bool,
    ) -> *const AsterStrHeader {
        if self.error.is_some() || !self.validate_string_builder(builder) {
            return ptr::null();
        }
        // SAFETY: the validated header owns `length <= capacity` initialized
        // UTF-8 bytes, formed only by appending validated ASTER strings.
        #[allow(unsafe_code)]
        let bytes = unsafe {
            if (*builder).length == 0 {
                &[]
            } else {
                std::slice::from_raw_parts((*builder).data, (*builder).length)
            }
        };
        let Ok(value) = std::str::from_utf8(bytes) else {
            self.fail("StringBuilder contains invalid UTF-8");
            return ptr::null();
        };
        if temporary {
            self.allocate_temporary_string_parts(&[value])
        } else {
            self.allocate_string_parts(&[value])
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn allocate_dictionary_in_region(
        &mut self,
        key_kind: DictionaryKeyKind,
        key_size: u32,
        key_align: u32,
        key_type_key: u64,
        value_size: u32,
        value_align: u32,
        value_type_key: u64,
        region: ListRegion,
    ) -> *mut AsterDictionary {
        if self.error.is_some() {
            return ptr::null_mut();
        }
        if key_size == 0 || value_size == 0 {
            self.fail("dictionary key and value sizes must be greater than zero");
            return ptr::null_mut();
        }
        if usize::try_from(key_size).ok() != Some(dictionary_key_kind_size(key_kind)) {
            self.fail("dictionary key size does not match its concrete key kind");
            return ptr::null_mut();
        }
        for (name, align) in [("key", key_align), ("value", value_align)] {
            if !align.is_power_of_two() || align as usize > MAX_ALIGN {
                self.fail(format!("dictionary {name} alignment must be a supported nonzero power of two, got {align}"));
                return ptr::null_mut();
            }
        }
        if region == ListRegion::Temporary && self.temporary_scopes.is_empty() {
            self.fail("temporary dictionary allocation requires an active temporary scope");
            return ptr::null_mut();
        }
        let Some(header) = self.allocate_in_region(
            size_of::<AsterDictionary>(),
            align_of::<AsterDictionary>(),
            region == ListRegion::Temporary,
        ) else {
            return ptr::null_mut();
        };
        #[allow(clippy::cast_ptr_alignment)]
        let header = header.cast::<AsterDictionary>();
        // SAFETY: fresh, aligned arena memory is unpublished until every field is initialized.
        #[allow(unsafe_code)]
        unsafe {
            *header = AsterDictionary {
                buckets: ptr::null_mut(),
                entries: ptr::null_mut(),
                length: 0,
                bucket_capacity: 0,
                entry_capacity: 0,
                entry_count: 0,
                key_size,
                key_align,
                value_size,
                value_align,
                key_type_key,
                value_type_key,
                owner_id: self.dictionary_owner_id,
                key_kind,
                region,
                birth_scope_depth: if region == ListRegion::Temporary {
                    u32::try_from(self.temporary_scopes.len()).unwrap_or(u32::MAX)
                } else {
                    0
                },
            };
        }
        self.record_allocation(AllocationCategory::Object, size_of::<AsterDictionary>());
        header
    }

    fn validate_dictionary_header(&mut self, dictionary: *const AsterDictionary) -> bool {
        #[allow(unsafe_code)]
        let (
            length,
            bucket_capacity,
            entry_capacity,
            count,
            buckets,
            entries,
            key_size,
            key_align,
            value_size,
            value_align,
            owner_id,
            key_kind,
        ) = unsafe {
            (
                (*dictionary).length,
                (*dictionary).bucket_capacity,
                (*dictionary).entry_capacity,
                (*dictionary).entry_count,
                (*dictionary).buckets,
                (*dictionary).entries,
                (*dictionary).key_size,
                (*dictionary).key_align,
                (*dictionary).value_size,
                (*dictionary).value_align,
                (*dictionary).owner_id,
                (*dictionary).key_kind,
            )
        };
        if length < 0 || count < 0 || length > count {
            self.fail("dictionary header has invalid length or entry count");
            return false;
        }
        if usize::try_from(key_size).ok() != Some(dictionary_key_kind_size(key_kind)) {
            self.fail("dictionary header key size does not match its key kind");
            return false;
        }
        if key_size == 0
            || value_size == 0
            || !key_align.is_power_of_two()
            || !value_align.is_power_of_two()
            || key_align as usize > MAX_ALIGN
            || value_align as usize > MAX_ALIGN
        {
            self.fail("dictionary header has invalid key or value layout metadata");
            return false;
        }
        if owner_id != self.dictionary_owner_id {
            self.fail("dictionary belongs to a different ExecutionContext");
            return false;
        }
        if bucket_capacity < 0 || entry_capacity < 0 || count > entry_capacity {
            self.fail("dictionary header has invalid capacity metadata");
            return false;
        }
        if bucket_capacity == 0 {
            if !buckets.is_null() || entry_capacity != 0 || !entries.is_null() || count != 0 {
                self.fail("empty dictionary header has inconsistent storage pointers");
                return false;
            }
        } else if bucket_capacity.count_ones() != 1
            || buckets.is_null()
            || entry_capacity <= 0
            || entries.is_null()
        {
            self.fail("dictionary header has invalid active storage");
            return false;
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_dictionary_operation(
        &mut self,
        dictionary: *const AsterDictionary,
        key_kind: DictionaryKeyKind,
        key_size: u32,
        key_align: u32,
        key_type_key: u64,
        value_size: u32,
        value_align: u32,
        value_type_key: u64,
        operation: &str,
    ) -> bool {
        if dictionary.is_null() {
            self.fail(format!("Dictionary.{operation} received a null Dictionary"));
            return false;
        }
        if !self.validate_dictionary_header(dictionary) {
            return false;
        }
        #[allow(unsafe_code)]
        let matches = unsafe {
            (*dictionary).key_kind == key_kind
                && (*dictionary).key_size == key_size
                && (*dictionary).key_align == key_align
                && (*dictionary).key_type_key == key_type_key
                && (*dictionary).value_size == value_size
                && (*dictionary).value_align == value_align
                && (*dictionary).value_type_key == value_type_key
        };
        if !matches {
            self.fail(format!(
                "Dictionary.{operation} concrete key/value metadata does not match the header"
            ));
        }
        matches
    }

    fn dictionary_key_bytes<'a>(
        &mut self,
        kind: DictionaryKeyKind,
        address: *const u8,
        key_size: u32,
        operation: &str,
    ) -> Option<&'a [u8]> {
        if address.is_null() {
            self.fail(format!(
                "Dictionary.{operation} received a null key address"
            ));
            return None;
        }
        if kind == DictionaryKeyKind::String {
            if usize::try_from(key_size).ok()? != size_of::<*const AsterStrHeader>() {
                self.fail(format!(
                    "Dictionary.{operation} string key layout has the wrong size"
                ));
                return None;
            }
            #[allow(unsafe_code)]
            let string = unsafe { ptr::read_unaligned(address.cast::<*const AsterStrHeader>()) };
            // SAFETY: generated code supplies a live ASTER string pointer.
            #[allow(unsafe_code)]
            let Some(value) = (unsafe { crate::string::view(string) }) else {
                self.fail(format!(
                    "Dictionary.{operation} received an invalid UTF-8 string key"
                ));
                return None;
            };
            return Some(value.as_bytes());
        }
        let expected_size = match kind {
            DictionaryKeyKind::Bool | DictionaryKeyKind::SByte | DictionaryKeyKind::Byte => 1,
            DictionaryKeyKind::Short | DictionaryKeyKind::UShort => 2,
            DictionaryKeyKind::Char | DictionaryKeyKind::Int | DictionaryKeyKind::UInt => 4,
            DictionaryKeyKind::Long | DictionaryKeyKind::ULong => 8,
            DictionaryKeyKind::String => unreachable!("handled above"),
        };
        if key_size != expected_size {
            self.fail(format!(
                "Dictionary.{operation} key size does not match its key kind"
            ));
            return None;
        }
        // SAFETY: generated code supplies a readable value slot of `key_size`
        // bytes. Integer and char values already use canonical little-endian
        // bit representations on the supported targets.
        #[allow(unsafe_code)]
        Some(unsafe { std::slice::from_raw_parts(address, usize::try_from(key_size).unwrap_or(0)) })
    }

    fn dictionary_hash_key(
        &mut self,
        kind: DictionaryKeyKind,
        address: *const u8,
        key_size: u32,
        operation: &str,
    ) -> Option<u64> {
        let bytes = self.dictionary_key_bytes(kind, address, key_size, operation)?;
        Some(siphash13(
            self.dictionary_hash_k0,
            self.dictionary_hash_k1,
            bytes,
        ))
    }

    fn dictionary_keys_equal(
        &mut self,
        kind: DictionaryKeyKind,
        left: *const u8,
        right: *const u8,
        key_size: u32,
        operation: &str,
    ) -> Option<bool> {
        let left = self.dictionary_key_bytes(kind, left, key_size, operation)?;
        let right = self.dictionary_key_bytes(kind, right, key_size, operation)?;
        Some(left == right)
    }

    fn dictionary_active_bytes(
        &mut self,
        bucket_capacity: i32,
        entry_capacity: i32,
        layout: DictionaryEntryLayout,
    ) -> Option<(usize, usize)> {
        let bucket_bytes = usize::try_from(bucket_capacity)
            .ok()?
            .checked_mul(size_of::<u32>())?;
        let entry_bytes = usize::try_from(entry_capacity)
            .ok()?
            .checked_mul(layout.stride)?;
        if bucket_bytes.checked_add(entry_bytes)? > DICTIONARY_MAX_ACTIVE_BYTES {
            self.fail("Dictionary active storage exceeds the 64 MiB limit");
            return None;
        }
        Some((bucket_bytes, entry_bytes))
    }

    fn dictionary_allocate_buffers(
        &mut self,
        bucket_capacity: i32,
        entry_capacity: i32,
        layout: DictionaryEntryLayout,
        region: ListRegion,
        birth_scope_depth: u32,
    ) -> Option<(*mut u32, *mut u8)> {
        let Some((bucket_bytes, entry_bytes)) =
            self.dictionary_active_bytes(bucket_capacity, entry_capacity, layout)
        else {
            if self.error.is_none() {
                self.fail("Dictionary buffer size overflow");
            }
            return None;
        };
        if region == ListRegion::Temporary && self.temporary_scopes.is_empty() {
            self.fail("temporary Dictionary growth requires an active temporary scope");
            return None;
        }
        // Use the temporary arena only when we are still inside the same scope
        // that created the header (scopes.len() == birth_scope_depth). If a
        // nested helper has pushed an additional scope, the buffers must go into
        // the permanent arena so they are not reclaimed when the helper exits.
        let use_temporary = region == ListRegion::Temporary
            && self.temporary_scopes.len() == birth_scope_depth as usize;
        // One aligned reservation keeps budget failure atomic: a rejected
        // entries buffer cannot leave a large buckets allocation behind.
        let Some(entries_offset) = checked_align_up(bucket_bytes, layout.align) else {
            self.fail("Dictionary buffer size overflow");
            return None;
        };
        let Some(allocation_bytes) = entries_offset.checked_add(entry_bytes) else {
            self.fail("Dictionary buffer size overflow");
            return None;
        };
        let allocation_align = align_of::<u32>().max(layout.align);
        let storage = self.allocate_in_region(allocation_bytes, allocation_align, use_temporary)?;
        let entries = storage.wrapping_add(entries_offset);
        #[allow(clippy::cast_ptr_alignment)]
        let buckets = storage.cast::<u32>();
        for index in 0..bucket_capacity {
            #[allow(unsafe_code)]
            unsafe {
                buckets
                    .add(usize::try_from(index).unwrap_or(0))
                    .write(DICTIONARY_EMPTY_BUCKET);
            }
        }
        self.record_allocation(AllocationCategory::Object, bucket_bytes);
        self.record_allocation(AllocationCategory::Object, entry_bytes);
        Some((buckets, entries))
    }

    fn dictionary_entry_pointer(
        entries: *mut u8,
        index: i32,
        layout: DictionaryEntryLayout,
    ) -> Option<*mut u8> {
        let offset = usize::try_from(index).ok()?.checked_mul(layout.stride)?;
        Some(entries.wrapping_add(offset))
    }

    #[allow(unsafe_code)]
    unsafe fn dictionary_entry_hash(entry: *const u8) -> u64 {
        unsafe { ptr::read_unaligned(entry.cast::<u64>()) }
    }

    #[allow(unsafe_code)]
    unsafe fn dictionary_entry_next(entry: *const u8) -> u32 {
        unsafe { ptr::read_unaligned(entry.add(8).cast::<u32>()) }
    }

    #[allow(unsafe_code)]
    unsafe fn dictionary_entry_live(entry: *const u8) -> u8 {
        unsafe { ptr::read(entry.add(12)) }
    }

    #[allow(unsafe_code)]
    unsafe fn set_dictionary_entry_next(entry: *mut u8, next: u32) {
        unsafe {
            ptr::write_unaligned(entry.add(8).cast::<u32>(), next);
        }
    }

    fn dictionary_find(
        &mut self,
        dictionary: *mut AsterDictionary,
        key: *const u8,
        hash: u64,
        operation: &str,
    ) -> Result<Option<(u32, u32, usize)>, ()> {
        #[allow(unsafe_code)]
        let (bucket_capacity, entry_count, buckets, entries, kind, key_size, layout) = unsafe {
            let Some(layout) = dictionary_entry_layout(
                (*dictionary).key_size,
                (*dictionary).key_align,
                (*dictionary).value_size,
                (*dictionary).value_align,
            ) else {
                self.fail(format!(
                    "Dictionary.{operation} has an invalid entry layout"
                ));
                return Err(());
            };
            (
                (*dictionary).bucket_capacity,
                (*dictionary).entry_count,
                (*dictionary).buckets,
                (*dictionary).entries,
                (*dictionary).key_kind,
                (*dictionary).key_size,
                layout,
            )
        };
        if bucket_capacity == 0 {
            return Ok(None);
        }
        let bucket =
            usize::try_from(hash & (u64::try_from(bucket_capacity).unwrap_or(1) - 1)).unwrap_or(0);
        #[allow(unsafe_code)]
        let mut current = unsafe { *buckets.add(bucket) };
        let mut previous = DICTIONARY_EMPTY_BUCKET;
        let mut steps = 0_i32;
        while current != DICTIONARY_EMPTY_BUCKET {
            if steps >= entry_count || current >= u32::try_from(entry_count).unwrap_or(0) {
                self.fail(format!(
                    "Dictionary.{operation} encountered an invalid or cyclic bucket chain"
                ));
                return Err(());
            }
            let Some(entry) = Self::dictionary_entry_pointer(
                entries,
                i32::try_from(current).unwrap_or(-1),
                layout,
            ) else {
                self.fail(format!("Dictionary.{operation} entry offset overflow"));
                return Err(());
            };
            #[allow(unsafe_code)]
            let (stored_hash, live, next) = unsafe {
                (
                    Self::dictionary_entry_hash(entry),
                    Self::dictionary_entry_live(entry),
                    Self::dictionary_entry_next(entry),
                )
            };
            if live > 1 {
                self.fail(format!(
                    "Dictionary.{operation} encountered an invalid live marker"
                ));
                return Err(());
            }
            if live == 1
                && stored_hash == hash
                && self
                    .dictionary_keys_equal(
                        kind,
                        entry.wrapping_add(layout.key_offset),
                        key,
                        key_size,
                        operation,
                    )
                    .ok_or(())?
            {
                return Ok(Some((current, previous, bucket)));
            }
            previous = current;
            current = next;
            steps += 1;
        }
        Ok(None)
    }

    fn dictionary_rebuild(
        &mut self,
        dictionary: *mut AsterDictionary,
        new_bucket_capacity: i32,
        new_entry_capacity: i32,
    ) -> bool {
        #[allow(unsafe_code)]
        let (old_entries, old_count, length, region, birth_scope_depth, layout) = unsafe {
            let Some(layout) = dictionary_entry_layout(
                (*dictionary).key_size,
                (*dictionary).key_align,
                (*dictionary).value_size,
                (*dictionary).value_align,
            ) else {
                self.fail("Dictionary rebuild has an invalid entry layout");
                return false;
            };
            (
                (*dictionary).entries,
                (*dictionary).entry_count,
                (*dictionary).length,
                (*dictionary).region,
                (*dictionary).birth_scope_depth,
                layout,
            )
        };
        if new_bucket_capacity.count_ones() != 1
            || new_entry_capacity < length
            || new_entry_capacity <= 0
        {
            self.fail("Dictionary rebuild requested invalid capacities");
            return false;
        }
        let Some((new_buckets, new_entries)) = self.dictionary_allocate_buffers(
            new_bucket_capacity,
            new_entry_capacity,
            layout,
            region,
            birth_scope_depth,
        ) else {
            return false;
        };
        let mut new_count = 0_i32;
        for old_index in 0..old_count {
            let Some(old_entry) = Self::dictionary_entry_pointer(old_entries, old_index, layout)
            else {
                self.fail("Dictionary rebuild source offset overflow");
                return false;
            };
            #[allow(unsafe_code)]
            let live = unsafe { Self::dictionary_entry_live(old_entry) };
            if live > 1 {
                self.fail("Dictionary rebuild found an invalid live marker");
                return false;
            }
            if live == 0 {
                continue;
            }
            let Some(new_entry) = Self::dictionary_entry_pointer(new_entries, new_count, layout)
            else {
                self.fail("Dictionary rebuild destination offset overflow");
                return false;
            };
            #[allow(unsafe_code)]
            unsafe {
                ptr::copy_nonoverlapping(old_entry, new_entry, layout.stride);
            }
            #[allow(unsafe_code)]
            let hash = unsafe { Self::dictionary_entry_hash(new_entry) };
            let bucket =
                usize::try_from(hash & (u64::try_from(new_bucket_capacity).unwrap_or(1) - 1))
                    .unwrap_or(0);
            #[allow(unsafe_code)]
            let head = unsafe { *new_buckets.add(bucket) };
            #[allow(unsafe_code)]
            unsafe {
                Self::set_dictionary_entry_next(new_entry, head);
                *new_buckets.add(bucket) = u32::try_from(new_count).unwrap_or(u32::MAX);
            }
            new_count += 1;
        }
        if new_count != length {
            self.fail("Dictionary rebuild live-entry count does not match Length");
            return false;
        }
        #[allow(unsafe_code)]
        unsafe {
            (*dictionary).buckets = new_buckets;
            (*dictionary).entries = new_entries;
            (*dictionary).bucket_capacity = new_bucket_capacity;
            (*dictionary).entry_capacity = new_entry_capacity;
            (*dictionary).entry_count = new_count;
        }
        true
    }

    fn dictionary_prepare_insert(&mut self, dictionary: *mut AsterDictionary) -> bool {
        #[allow(unsafe_code)]
        let (length, count, entry_capacity, bucket_capacity) = unsafe {
            (
                (*dictionary).length,
                (*dictionary).entry_count,
                (*dictionary).entry_capacity,
                (*dictionary).bucket_capacity,
            )
        };
        let Some(new_length) = length.checked_add(1) else {
            self.fail("Dictionary Length overflow");
            return false;
        };
        if new_length > DICTIONARY_MAX_ENTRIES {
            self.fail("Dictionary exceeds the maximum of 100000 live entries");
            return false;
        }
        if bucket_capacity == 0 {
            return self.dictionary_rebuild(
                dictionary,
                DICTIONARY_INITIAL_CAPACITY,
                DICTIONARY_INITIAL_CAPACITY,
            );
        }
        let load_limit = bucket_capacity.saturating_mul(3) / 4;
        let needs_bucket_growth = new_length > load_limit;
        let needs_entry_growth = count == entry_capacity;
        if !needs_bucket_growth && !needs_entry_growth {
            return true;
        }
        let new_bucket_capacity = if needs_bucket_growth {
            let Some(capacity) = bucket_capacity.checked_mul(2) else {
                self.fail("Dictionary bucket capacity overflow");
                return false;
            };
            capacity
        } else {
            bucket_capacity
        };
        let tombstones = count - length;
        let new_entry_capacity = if needs_entry_growth {
            let doubled = entry_capacity.checked_mul(2);
            let sparse_tombstones = tombstones.saturating_mul(2) < length;
            #[allow(unsafe_code)]
            let layout = unsafe {
                dictionary_entry_layout(
                    (*dictionary).key_size,
                    (*dictionary).key_align,
                    (*dictionary).value_size,
                    (*dictionary).value_align,
                )
                .expect("validated dictionary layout")
            };
            if let Some(capacity) = doubled
                && (tombstones == 0
                    || sparse_tombstones
                        && dictionary_storage_fits(new_bucket_capacity, capacity, layout))
            {
                capacity
            } else if tombstones > 0 {
                entry_capacity
            } else {
                self.fail("Dictionary entry capacity overflow");
                return false;
            }
        } else {
            entry_capacity
        };
        self.dictionary_rebuild(dictionary, new_bucket_capacity, new_entry_capacity)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn dictionary_add_or_set(
        &mut self,
        dictionary: *mut AsterDictionary,
        key_kind: DictionaryKeyKind,
        key_size: u32,
        key_align: u32,
        key_type_key: u64,
        value_size: u32,
        value_align: u32,
        value_type_key: u64,
        key: *const u8,
        value: *const u8,
        replace: bool,
    ) -> i8 {
        if self.error.is_some() {
            return 0;
        }
        let operation = if replace { "Set" } else { "Add" };
        if value.is_null() {
            self.fail(format!(
                "Dictionary.{operation} received a null value address"
            ));
            return 0;
        }
        if !self.validate_dictionary_operation(
            dictionary,
            key_kind,
            key_size,
            key_align,
            key_type_key,
            value_size,
            value_align,
            value_type_key,
            operation,
        ) {
            return 0;
        }
        let Some(hash) = self.dictionary_hash_key(key_kind, key, key_size, operation) else {
            return 0;
        };
        let Ok(found) = self.dictionary_find(dictionary, key, hash, operation) else {
            return 0;
        };
        if let Some((index, _, _)) = found {
            if replace {
                #[allow(unsafe_code)]
                let (entries, layout) = unsafe {
                    (
                        (*dictionary).entries,
                        dictionary_entry_layout(key_size, key_align, value_size, value_align)
                            .expect("validated layout"),
                    )
                };
                let Some(entry) = Self::dictionary_entry_pointer(
                    entries,
                    i32::try_from(index).unwrap_or(-1),
                    layout,
                ) else {
                    self.fail("Dictionary.Set entry offset overflow");
                    return 0;
                };
                #[allow(unsafe_code)]
                unsafe {
                    ptr::copy(
                        value,
                        entry.add(layout.value_offset),
                        usize::try_from(value_size).unwrap_or(0),
                    );
                }
                return 1;
            }
            return 0;
        }
        if !self.dictionary_prepare_insert(dictionary) {
            return 0;
        }
        #[allow(unsafe_code)]
        let (entries, buckets, count, length, bucket_capacity, layout) = unsafe {
            (
                (*dictionary).entries,
                (*dictionary).buckets,
                (*dictionary).entry_count,
                (*dictionary).length,
                (*dictionary).bucket_capacity,
                dictionary_entry_layout(key_size, key_align, value_size, value_align)
                    .expect("validated layout"),
            )
        };
        let Some(entry) = Self::dictionary_entry_pointer(entries, count, layout) else {
            self.fail(format!("Dictionary.{operation} entry offset overflow"));
            return 0;
        };
        let bucket =
            usize::try_from(hash & (u64::try_from(bucket_capacity).unwrap_or(1) - 1)).unwrap_or(0);
        #[allow(unsafe_code)]
        let head = unsafe { *buckets.add(bucket) };
        #[allow(unsafe_code)]
        unsafe {
            ptr::write_unaligned(entry.cast::<u64>(), hash);
            Self::set_dictionary_entry_next(entry, head);
            ptr::write(entry.add(12), 1);
            ptr::copy_nonoverlapping(
                key,
                entry.add(layout.key_offset),
                usize::try_from(key_size).unwrap_or(0),
            );
            ptr::copy_nonoverlapping(
                value,
                entry.add(layout.value_offset),
                usize::try_from(value_size).unwrap_or(0),
            );
            *buckets.add(bucket) = u32::try_from(count).unwrap_or(u32::MAX);
            (*dictionary).entry_count = count + 1;
            (*dictionary).length = length + 1;
        }
        i8::from(!replace)
    }

    #[allow(clippy::too_many_arguments)]
    fn dictionary_contains_or_remove(
        &mut self,
        dictionary: *mut AsterDictionary,
        key_kind: DictionaryKeyKind,
        key_size: u32,
        key_align: u32,
        key_type_key: u64,
        value_size: u32,
        value_align: u32,
        value_type_key: u64,
        key: *const u8,
        remove: bool,
    ) -> i8 {
        if self.error.is_some() {
            return 0;
        }
        let operation = if remove { "Remove" } else { "ContainsKey" };
        if !self.validate_dictionary_operation(
            dictionary,
            key_kind,
            key_size,
            key_align,
            key_type_key,
            value_size,
            value_align,
            value_type_key,
            operation,
        ) {
            return 0;
        }
        let Some(hash) = self.dictionary_hash_key(key_kind, key, key_size, operation) else {
            return 0;
        };
        let Ok(found) = self.dictionary_find(dictionary, key, hash, operation) else {
            return 0;
        };
        let Some((index, previous, bucket)) = found else {
            return 0;
        };
        if !remove {
            return 1;
        }
        #[allow(unsafe_code)]
        let (entries, buckets, layout, length) = unsafe {
            (
                (*dictionary).entries,
                (*dictionary).buckets,
                dictionary_entry_layout(key_size, key_align, value_size, value_align)
                    .expect("validated layout"),
                (*dictionary).length,
            )
        };
        let entry =
            Self::dictionary_entry_pointer(entries, i32::try_from(index).unwrap_or(-1), layout)
                .expect("validated index");
        #[allow(unsafe_code)]
        let next = unsafe { Self::dictionary_entry_next(entry) };
        #[allow(unsafe_code)]
        unsafe {
            if previous == DICTIONARY_EMPTY_BUCKET {
                *buckets.add(bucket) = next;
            } else {
                let previous_entry = Self::dictionary_entry_pointer(
                    entries,
                    i32::try_from(previous).unwrap_or(-1),
                    layout,
                )
                .expect("validated previous index");
                Self::set_dictionary_entry_next(previous_entry, next);
            }
            ptr::write(entry.add(12), 0);
            (*dictionary).length = length - 1;
        }
        1
    }

    #[allow(clippy::too_many_arguments)]
    fn dictionary_try_get(
        &mut self,
        dictionary: *mut AsterDictionary,
        key_kind: DictionaryKeyKind,
        key_size: u32,
        key_align: u32,
        key_type_key: u64,
        value_size: u32,
        value_align: u32,
        value_type_key: u64,
        key: *const u8,
        destination: *mut u8,
        total_size: u32,
        some_tag: u32,
        none_tag: u32,
        payload_offset: u32,
    ) {
        if self.error.is_some() {
            return;
        }
        if destination.is_null() {
            self.fail("Dictionary.TryGet received a null destination");
            return;
        }
        if !self.validate_dictionary_operation(
            dictionary,
            key_kind,
            key_size,
            key_align,
            key_type_key,
            value_size,
            value_align,
            value_type_key,
            "TryGet",
        ) {
            return;
        }
        let Some(payload_end) = payload_offset.checked_add(value_size) else {
            self.fail("Dictionary.TryGet Option payload layout overflow");
            return;
        };
        if total_size < 4 || payload_end > total_size {
            self.fail("Dictionary.TryGet Option payload lies outside its destination");
            return;
        }
        let Some(hash) = self.dictionary_hash_key(key_kind, key, key_size, "TryGet") else {
            return;
        };
        let Ok(found) = self.dictionary_find(dictionary, key, hash, "TryGet") else {
            return;
        };
        #[allow(unsafe_code)]
        unsafe {
            ptr::write_bytes(destination, 0, usize::try_from(total_size).unwrap_or(0));
        }
        let tag = if let Some((index, _, _)) = found {
            #[allow(unsafe_code)]
            let (entries, layout) = unsafe {
                (
                    (*dictionary).entries,
                    dictionary_entry_layout(key_size, key_align, value_size, value_align)
                        .expect("validated layout"),
                )
            };
            let entry =
                Self::dictionary_entry_pointer(entries, i32::try_from(index).unwrap_or(-1), layout)
                    .expect("validated index");
            #[allow(unsafe_code)]
            unsafe {
                ptr::copy_nonoverlapping(
                    entry.add(layout.value_offset),
                    destination.add(usize::try_from(payload_offset).unwrap_or(0)),
                    usize::try_from(value_size).unwrap_or(0),
                );
            }
            some_tag
        } else {
            none_tag
        };
        #[allow(unsafe_code)]
        unsafe {
            ptr::write_unaligned(destination.cast::<u32>(), tag);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dictionary_entries(
        &mut self,
        dictionary: *mut AsterDictionary,
        key_kind: DictionaryKeyKind,
        key_size: u32,
        key_align: u32,
        key_type_key: u64,
        value_size: u32,
        value_align: u32,
        value_type_key: u64,
        entry_size: u32,
        key_offset: u32,
        value_offset: u32,
        region: ListRegion,
    ) -> *mut AsterArray {
        if self.error.is_some() {
            return ptr::null_mut();
        }
        if !self.validate_dictionary_operation(
            dictionary,
            key_kind,
            key_size,
            key_align,
            key_type_key,
            value_size,
            value_align,
            value_type_key,
            "Entries",
        ) {
            return ptr::null_mut();
        }
        let Some(key_end) = key_offset.checked_add(key_size) else {
            self.fail("Dictionary.Entries key field layout overflow");
            return ptr::null_mut();
        };
        let Some(value_end) = value_offset.checked_add(value_size) else {
            self.fail("Dictionary.Entries value field layout overflow");
            return ptr::null_mut();
        };
        if entry_size == 0
            || key_end > entry_size
            || value_end > entry_size
            || key_offset % key_align != 0
            || value_offset % value_align != 0
        {
            self.fail("Dictionary.Entries has invalid DictionaryEntry field layout");
            return ptr::null_mut();
        }
        #[allow(unsafe_code)]
        let (length, count, entries, layout) = unsafe {
            (
                (*dictionary).length,
                (*dictionary).entry_count,
                (*dictionary).entries,
                dictionary_entry_layout(key_size, key_align, value_size, value_align)
                    .expect("validated layout"),
            )
        };
        let mut live = 0_i32;
        for index in 0..count {
            let entry =
                Self::dictionary_entry_pointer(entries, index, layout).expect("validated index");
            #[allow(unsafe_code)]
            let marker = unsafe { Self::dictionary_entry_live(entry) };
            if marker > 1 {
                self.fail("Dictionary.Entries encountered an invalid live marker");
                return ptr::null_mut();
            }
            live += i32::from(marker);
        }
        if live != length {
            self.fail("Dictionary.Entries live-entry count does not match Length");
            return ptr::null_mut();
        }
        let array =
            self.allocate_array_in_region(length, entry_size, region == ListRegion::Temporary);
        if array.is_null() {
            return ptr::null_mut();
        }
        #[allow(unsafe_code)]
        let output = unsafe { (*array).data };
        let mut output_index = 0_i32;
        for index in 0..count {
            let entry =
                Self::dictionary_entry_pointer(entries, index, layout).expect("validated index");
            #[allow(unsafe_code)]
            let marker = unsafe { Self::dictionary_entry_live(entry) };
            if marker == 0 {
                continue;
            }
            let destination_offset = usize::try_from(output_index)
                .ok()
                .and_then(|index| index.checked_mul(usize::try_from(entry_size).ok()?))
                .expect("validated snapshot size");
            #[allow(unsafe_code)]
            unsafe {
                ptr::copy_nonoverlapping(
                    entry.add(layout.key_offset),
                    output
                        .add(destination_offset)
                        .add(usize::try_from(key_offset).unwrap_or(0)),
                    usize::try_from(key_size).unwrap_or(0),
                );
                ptr::copy_nonoverlapping(
                    entry.add(layout.value_offset),
                    output
                        .add(destination_offset)
                        .add(usize::try_from(value_offset).unwrap_or(0)),
                    usize::try_from(value_size).unwrap_or(0),
                );
            }
            output_index += 1;
        }
        array
    }

    /// Every invariant a well-formed `AsterList` header must satisfy,
    /// independent of any particular operation. Shared by `Length` and `Add`
    /// so there is exactly one definition of "a valid list header" â€” never
    /// duplicated, never re-derived ad hoc at each call site.
    ///
    /// # Safety
    ///
    /// `list` must be non-null and point to a header this runtime allocated.
    fn validate_list_header(&mut self, list: *const AsterList) -> bool {
        // SAFETY: caller guarantees `list` is non-null and runtime-owned;
        // every field is read once, transiently, into locals below.
        #[allow(unsafe_code)]
        let (length, capacity, element_size, element_align, data_is_null) = unsafe {
            (
                (*list).length,
                (*list).capacity,
                (*list).element_size,
                (*list).element_align,
                (*list).data.is_null(),
            )
        };
        if length < 0 {
            self.fail("list header has a negative length");
            return false;
        }
        if capacity < 0 {
            self.fail("list header has a negative capacity");
            return false;
        }
        if length > capacity {
            self.fail("list header has a length greater than its capacity");
            return false;
        }
        if element_size == 0 {
            self.fail("list header has a zero element size");
            return false;
        }
        if !element_align.is_power_of_two() {
            self.fail("list header has a non-power-of-two element alignment");
            return false;
        }
        if element_align as usize > MAX_ALIGN {
            self.fail(format!(
                "list header element alignment {element_align} exceeds the arena's maximum supported alignment of {MAX_ALIGN}"
            ));
            return false;
        }
        if capacity == 0 {
            if !data_is_null {
                self.fail("list header has a data pointer despite zero capacity");
                return false;
            }
        } else if data_is_null {
            self.fail("list header has a null data pointer despite a positive capacity");
            return false;
        }
        true
    }

    /// Grows a list's buffer geometrically (`0 -> 4`, then doubling) and
    /// copies the `length` existing elements into it. Returns `None` (after
    /// calling `self.fail`) on any overflow, missing temporary scope, or
    /// arena failure; the caller must return immediately in that case,
    /// leaving the header untouched.
    #[allow(clippy::too_many_arguments)]
    fn grow_list_buffer(
        &mut self,
        data: *mut u8,
        length: i32,
        capacity: i32,
        element_size: u32,
        element_align: u32,
        region: ListRegion,
        birth_scope_depth: u32,
    ) -> Option<(*mut u8, i32)> {
        let new_capacity = if capacity == 0 {
            Some(4_i32)
        } else {
            capacity.checked_mul(2)
        };
        let Some(new_capacity) = new_capacity else {
            self.fail("list capacity overflow while growing");
            return None;
        };
        let Ok(new_capacity_usize) = usize::try_from(new_capacity) else {
            self.fail("list capacity overflow while growing");
            return None;
        };
        let Some(byte_size) = checked_allocation_size(
            new_capacity_usize,
            usize::try_from(element_size).unwrap_or(0),
        ) else {
            self.fail("list buffer size overflow while growing");
            return None;
        };
        if byte_size == 0 {
            self.fail("list buffer size overflow while growing");
            return None;
        }
        if region == ListRegion::Temporary && self.temporary_scopes.is_empty() {
            self.fail("temporary list growth requires an active temporary scope");
            return None;
        }
        if element_align as usize > MAX_ALIGN {
            self.fail(format!(
                "list element alignment {element_align} exceeds the arena's maximum supported alignment of {MAX_ALIGN}"
            ));
            return None;
        }
        // Use the temporary arena only when we are still inside the same scope
        // that created the header (scopes.len() == birth_scope_depth). If a
        // nested helper has pushed an additional scope, the buffers must go into
        // the permanent arena so they are not reclaimed when the helper exits.
        let use_temporary = region == ListRegion::Temporary
            && self.temporary_scopes.len() == birth_scope_depth as usize;
        let align = usize::try_from(element_align).unwrap_or(1);
        let new_data = self.allocate_in_region(byte_size, align, use_temporary)?;
        let old_byte_len =
            usize::try_from(length).unwrap_or(0) * usize::try_from(element_size).unwrap_or(0);
        if !data.is_null() && old_byte_len > 0 {
            // SAFETY: `data` (the previous buffer) holds `old_byte_len` valid
            // bytes, already validated by the caller; `new_data` is
            // `byte_size >= old_byte_len` freshly allocated bytes with no
            // other reference to them yet.
            #[allow(unsafe_code)]
            unsafe {
                ptr::copy_nonoverlapping(data, new_data, old_byte_len);
            }
        }
        self.record_allocation(AllocationCategory::Object, byte_size);
        Some((new_data, new_capacity))
    }

    /// Appends one element to `list`'s buffer, growing it geometrically
    /// (`0 -> 4`, then doubling) when full. Every failure is reported
    /// through `self.fail` and leaves the header exactly as it was before
    /// this call (`length` unchanged, no partial growth applied) â€” never a
    /// panic, a trap, or a partially updated header. `source` needs to
    /// remain valid only for the duration of this call; nothing about it is
    /// retained past it.
    fn list_add(
        &mut self,
        list: *mut AsterList,
        expected_element_size: u32,
        expected_element_align: u32,
        expected_element_type_key: u64,
        source: *const u8,
    ) {
        if list.is_null() {
            self.fail("list.Add received a null list");
            return;
        }
        if source.is_null() {
            self.fail("list.Add received a null source value");
            return;
        }
        if !self.validate_list_header(list) {
            return;
        }
        // SAFETY: `list` was just validated above; every field is read once,
        // transiently, into locals below.
        #[allow(unsafe_code)]
        let (
            length,
            capacity,
            element_size,
            element_align,
            element_type_key,
            region,
            birth_scope_depth,
            data,
        ) = unsafe {
            (
                (*list).length,
                (*list).capacity,
                (*list).element_size,
                (*list).element_align,
                (*list).element_type_key,
                (*list).region,
                (*list).birth_scope_depth,
                (*list).data,
            )
        };
        if expected_element_size != element_size {
            self.fail(format!(
                "list.Add element size mismatch: expected {expected_element_size}, header has {element_size}"
            ));
            return;
        }
        if expected_element_align != element_align {
            self.fail(format!(
                "list.Add element alignment mismatch: expected {expected_element_align}, header has {element_align}"
            ));
            return;
        }
        if expected_element_type_key != element_type_key {
            self.fail("list.Add element type key mismatch");
            return;
        }

        let (data, capacity) = if length == capacity {
            let Some(grown) = self.grow_list_buffer(
                data,
                length,
                capacity,
                element_size,
                element_align,
                region,
                birth_scope_depth,
            ) else {
                return;
            };
            grown
        } else {
            (data, capacity)
        };

        let Some(new_length) = length.checked_add(1) else {
            self.fail("list length overflow");
            return;
        };
        let offset =
            usize::try_from(length).unwrap_or(0) * usize::try_from(element_size).unwrap_or(0);
        // SAFETY: `data` has room for at least `length + 1` elements of
        // `element_size` bytes (unchanged with spare capacity, or freshly
        // grown above); `source` was validated non-null and the caller
        // guarantees it is readable for `element_size` bytes for the
        // duration of this call.
        #[allow(unsafe_code)]
        unsafe {
            ptr::copy_nonoverlapping(
                source,
                data.add(offset),
                usize::try_from(element_size).unwrap_or(0),
            );
        }

        // SAFETY: `list` was validated above; every field is updated
        // together, only after the new element was fully written, so no
        // partially updated header is ever observable.
        #[allow(unsafe_code)]
        unsafe {
            (*list).data = data;
            (*list).capacity = capacity;
            (*list).length = new_length;
            (*list).version = (*list).version.wrapping_add(1);
        }
    }

    /// Copies element `index`'s bytes into `destination`. Never modifies
    /// `list` (no growth, no length/capacity change), never returns a
    /// pointer into the buffer, and never retains `destination` past this
    /// call. Every failure is reported through `self.fail`; `destination`
    /// is left untouched on failure.
    fn list_get(
        &mut self,
        list: *const AsterList,
        expected_element_size: u32,
        expected_element_align: u32,
        expected_element_type_key: u64,
        index: i32,
        destination: *mut u8,
    ) {
        if list.is_null() {
            self.fail("list.Get received a null list");
            return;
        }
        if destination.is_null() {
            self.fail("list.Get received a null destination");
            return;
        }
        if !self.validate_list_header(list) {
            return;
        }
        // SAFETY: `list` was just validated above; every field is read once,
        // transiently, into locals below.
        #[allow(unsafe_code)]
        let (length, element_size, element_align, element_type_key, data) = unsafe {
            (
                (*list).length,
                (*list).element_size,
                (*list).element_align,
                (*list).element_type_key,
                (*list).data,
            )
        };
        if expected_element_size != element_size {
            self.fail(format!(
                "list.Get element size mismatch: expected {expected_element_size}, header has {element_size}"
            ));
            return;
        }
        if expected_element_align != element_align {
            self.fail(format!(
                "list.Get element alignment mismatch: expected {expected_element_align}, header has {element_align}"
            ));
            return;
        }
        if expected_element_type_key != element_type_key {
            self.fail("list.Get element type key mismatch");
            return;
        }
        if index < 0 {
            self.fail(format!(
                "List.Get index {index} is negative (length {length})"
            ));
            return;
        }
        if index >= length {
            self.fail(format!(
                "List.Get index {index} is out of bounds for length {length}"
            ));
            return;
        }
        let Some(offset) = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(usize::try_from(element_size).unwrap_or(0)))
        else {
            self.fail("list.Get index * element size overflow");
            return;
        };
        // SAFETY: `0 <= index < length` and the header's invariants (checked
        // by `validate_list_header`) guarantee `data` holds `length *
        // element_size` valid bytes, so `offset..offset+element_size` lies
        // inside it; `destination` was validated non-null and the caller
        // guarantees it is writable for `element_size` bytes for the
        // duration of this call.
        #[allow(unsafe_code)]
        unsafe {
            ptr::copy_nonoverlapping(
                data.add(offset),
                destination,
                usize::try_from(element_size).unwrap_or(0),
            );
        }
    }

    /// Computes every checked offset `RemoveAt` needs: the removed slot's
    /// offset, the offset of the first element after it, how many bytes to
    /// shift, the new `length`, and the offset of the slot that falls out of
    /// range once `length` shrinks. Returns `None` (after calling
    /// `self.fail`) on any overflow — the caller must return immediately
    /// without touching the header or buffer.
    fn compute_remove_at_offsets(
        &mut self,
        length: i32,
        index: i32,
        element_size_usize: usize,
    ) -> Option<(usize, usize, usize, i32, usize)> {
        let Some(index_offset) = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(element_size_usize))
        else {
            self.fail("list.RemoveAt index * element size overflow");
            return None;
        };
        let Some(next_index) = index.checked_add(1) else {
            self.fail("list.RemoveAt index overflow");
            return None;
        };
        let Some(source_offset) = usize::try_from(next_index)
            .ok()
            .and_then(|next_index| next_index.checked_mul(element_size_usize))
        else {
            self.fail("list.RemoveAt (index + 1) * element size overflow");
            return None;
        };
        let Some(remaining) = length.checked_sub(next_index) else {
            self.fail("list.RemoveAt remaining element count underflow");
            return None;
        };
        let Some(remaining_usize) = usize::try_from(remaining).ok() else {
            self.fail("list.RemoveAt remaining element count overflow");
            return None;
        };
        let Some(move_bytes) = remaining_usize.checked_mul(element_size_usize) else {
            self.fail("list.RemoveAt byte count overflow");
            return None;
        };
        let Some(new_length) = length.checked_sub(1) else {
            self.fail("list.RemoveAt length underflow");
            return None;
        };
        let Some(old_last_offset) = usize::try_from(new_length)
            .ok()
            .and_then(|new_length| new_length.checked_mul(element_size_usize))
        else {
            self.fail("list.RemoveAt old last slot offset overflow");
            return None;
        };
        Some((
            index_offset,
            source_offset,
            move_bytes,
            new_length,
            old_last_offset,
        ))
    }

    /// Removes element `index`, shifting every later element one slot left
    /// with an overlap-safe move (source/destination ranges can overlap, so
    /// `memcpy`/`copy_nonoverlapping` would be UB), then zeroes the vacated
    /// last slot and decrements `length`. Never allocates, never touches
    /// `capacity`/`data`. Every failure is reported through `self.fail` and
    /// leaves the header and buffer exactly as they were (every check runs
    /// before any byte is written).
    fn list_remove_at(
        &mut self,
        list: *mut AsterList,
        expected_element_size: u32,
        expected_element_align: u32,
        expected_element_type_key: u64,
        index: i32,
    ) {
        if list.is_null() {
            self.fail("list.RemoveAt received a null list");
            return;
        }
        if !self.validate_list_header(list) {
            return;
        }
        // SAFETY: `list` was just validated above; every field is read once,
        // transiently, into locals below.
        #[allow(unsafe_code)]
        let (length, element_size, element_align, element_type_key, data) = unsafe {
            (
                (*list).length,
                (*list).element_size,
                (*list).element_align,
                (*list).element_type_key,
                (*list).data,
            )
        };
        if expected_element_size != element_size {
            self.fail(format!(
                "list.RemoveAt element size mismatch: expected {expected_element_size}, header has {element_size}"
            ));
            return;
        }
        if expected_element_align != element_align {
            self.fail(format!(
                "list.RemoveAt element alignment mismatch: expected {expected_element_align}, header has {element_align}"
            ));
            return;
        }
        if expected_element_type_key != element_type_key {
            self.fail("list.RemoveAt element type key mismatch");
            return;
        }
        if index < 0 {
            self.fail(format!(
                "List.RemoveAt index {index} is negative (length {length})"
            ));
            return;
        }
        if index >= length {
            self.fail(format!(
                "List.RemoveAt index {index} is out of bounds for length {length}"
            ));
            return;
        }

        let element_size_usize = usize::try_from(element_size).unwrap_or(0);
        let Some((index_offset, source_offset, move_bytes, new_length, old_last_offset)) =
            self.compute_remove_at_offsets(length, index, element_size_usize)
        else {
            return;
        };

        if move_bytes > 0 {
            // SAFETY: `source_offset..source_offset+move_bytes` and
            // `index_offset..index_offset+move_bytes` both lie within the
            // buffer (bounded by `length * element_size`, guaranteed by the
            // header invariants and `index < length` checked above); the two
            // ranges may overlap, so `ptr::copy` (memmove-equivalent) is used
            // instead of `copy_nonoverlapping`.
            #[allow(unsafe_code)]
            unsafe {
                ptr::copy(data.add(source_offset), data.add(index_offset), move_bytes);
            }
        }
        // SAFETY: `old_last_offset` is the last valid element's offset before
        // this removal (`(length - 1) * element_size`), still within the
        // buffer; zeroing it after the move and before publishing the new
        // `length` below is safe since it is about to fall outside the new
        // `length` and is never observed by the language.
        #[allow(unsafe_code)]
        unsafe {
            ptr::write_bytes(data.add(old_last_offset), 0, element_size_usize);
        }
        // SAFETY: `list` was validated above; `data`/`capacity` are
        // untouched, only `length` changes, and only after the shift and
        // clear above have both fully completed.
        #[allow(unsafe_code)]
        unsafe {
            (*list).length = new_length;
            (*list).version = (*list).version.wrapping_add(1);
        }
    }

    pub(crate) fn allocate_object(&mut self, size: u32) -> *mut u8 {
        let bytes = usize::try_from(size.max(1)).unwrap_or(1);
        let Some(pointer) = self.allocate_in_region(bytes, 8, false) else {
            return ptr::null_mut();
        };
        self.record_allocation(AllocationCategory::Object, bytes);
        pointer
    }

    fn allocate_string_parts_in_region(
        &mut self,
        parts: &[&str],
        temporary: bool,
    ) -> *const AsterStrHeader {
        if temporary && self.temporary_scopes.is_empty() {
            self.fail("temporary string allocation requires an active temporary scope");
            return ptr::null();
        }

        let Some(payload_bytes) = parts
            .iter()
            .try_fold(0_usize, |total, part| total.checked_add(part.len()))
        else {
            self.fail("string concatenation exceeds the addressable range");
            return ptr::null();
        };
        let Some(total_bytes) = size_of::<usize>().checked_add(payload_bytes) else {
            self.fail("string allocation exceeds the addressable range");
            return ptr::null();
        };

        let Some(pointer) =
            self.allocate_in_region(total_bytes, align_of::<AsterStrHeader>(), temporary)
        else {
            return ptr::null();
        };

        // SAFETY: `pointer` points to `total_bytes` of zeroed, correctly aligned
        // memory owned by the selected arena. No other reference to this region
        // exists. The slice is consumed before `record_allocation` borrows self.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts_mut(pointer, total_bytes) };
        bytes[..size_of::<usize>()].copy_from_slice(&payload_bytes.to_ne_bytes());
        let mut cursor = size_of::<usize>();
        for part in parts {
            let end = cursor + part.len();
            bytes[cursor..end].copy_from_slice(part.as_bytes());
            cursor = end;
        }
        self.record_allocation(AllocationCategory::String, total_bytes);
        // The arena allocates with `align_of::<AsterStrHeader>()`, so the
        // pointer is correctly aligned for this cast.
        #[allow(clippy::cast_ptr_alignment)]
        pointer.cast::<AsterStrHeader>()
    }

    pub(crate) fn allocate_string_parts(&mut self, parts: &[&str]) -> *const AsterStrHeader {
        self.allocate_string_parts_in_region(parts, false)
    }

    pub(crate) fn allocate_temporary_string_parts(
        &mut self,
        parts: &[&str],
    ) -> *const AsterStrHeader {
        self.allocate_string_parts_in_region(parts, true)
    }
}

fn sum_known_u64(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    left?.checked_add(right?)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_temporary_scope_enter(context: *mut ExecutionContext) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.enter_temporary_scope();
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_temporary_scope_leave(context: *mut ExecutionContext) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.leave_temporary_scope();
}

/// Enter one compiler-authorized fine-grained Temporary subregion.
///
/// This private JIT ABI does not alter semantic function Temporary-scope
/// depth. Malformed ordering is reported through [`ExecutionContext::fail`].
#[doc(hidden)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub(crate) extern "C" fn aster_rt_temporary_subregion_enter(context: *mut ExecutionContext) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.enter_temporary_subregion();
}

/// Rewind the active compiler-authorized fine-grained Temporary subregion.
///
/// Cleanup deliberately remains effective after an earlier controlled error.
#[doc(hidden)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub(crate) extern "C" fn aster_rt_temporary_subregion_exit(context: *mut ExecutionContext) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.leave_temporary_subregion();
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_call_enter(context: *mut ExecutionContext) -> i8 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    i8::from(context.enter_call())
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_call_leave(context: *mut ExecutionContext) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.leave_call();
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[must_use]
pub extern "C" fn aster_rt_has_error(context: *const ExecutionContext) -> i8 {
    if context.is_null() {
        return 1;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &*context };
    i8::from(context.error.is_some())
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_array_new(
    context: *mut ExecutionContext,
    length: i32,
    element_size: i32,
) -> *mut AsterArray {
    if context.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let size = u32::try_from(element_size).unwrap_or(1);
    context.allocate_array(length, size)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_array_new_temporary(
    context: *mut ExecutionContext,
    length: i32,
    element_size: i32,
) -> *mut AsterArray {
    if context.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let size = u32::try_from(element_size).unwrap_or(1);
    context.allocate_temporary_array(length, size)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_array_element(
    context: *mut ExecutionContext,
    array: *mut AsterArray,
    index: i32,
) -> *mut u8 {
    if context.is_null() || array.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: both pointers are produced and kept alive by the same execution
    // context; generated code cannot manufacture or retain either pointer.
    #[allow(unsafe_code)]
    let (context, array) = unsafe { (&mut *context, &*array) };
    if index < 0 || index >= array.length {
        context.fail(format!(
            "array index {index} is outside the valid range 0..{}",
            array.length
        ));
        return array.data;
    }
    let Ok(index) = usize::try_from(index) else {
        return array.data;
    };
    array.data.wrapping_add(index * array.element_size as usize)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_array_length(
    context: *mut ExecutionContext,
    array: *const AsterArray,
) -> i32 {
    if context.is_null() || array.is_null() {
        return 0;
    }
    // SAFETY: array headers are owned by the live context passed alongside it.
    #[allow(unsafe_code)]
    unsafe {
        (*array).length
    }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_list_new(
    context: *mut ExecutionContext,
    element_size: i32,
    element_align: i32,
    element_type_key: i64,
) -> *mut AsterList {
    if context.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let size = u32::try_from(element_size).unwrap_or(0);
    let align = u32::try_from(element_align).unwrap_or(0);
    // `as u64` is a bit-preserving reinterpretation of the same 64 bits the
    // compiler produced from `aster_mir::type_key`; the wire type is `i64`
    // only because that is the runtime ABI's signed 64-bit carrier.
    #[allow(clippy::cast_sign_loss)]
    let type_key = element_type_key as u64;
    context.allocate_list_in_region(size, align, type_key, ListRegion::Persistent)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_list_new_temporary(
    context: *mut ExecutionContext,
    element_size: i32,
    element_align: i32,
    element_type_key: i64,
) -> *mut AsterList {
    if context.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let size = u32::try_from(element_size).unwrap_or(0);
    let align = u32::try_from(element_align).unwrap_or(0);
    #[allow(clippy::cast_sign_loss)]
    let type_key = element_type_key as u64;
    context.allocate_list_in_region(size, align, type_key, ListRegion::Temporary)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_builder_new(
    context: *mut ExecutionContext,
) -> *mut AsterStringBuilder {
    if context.is_null() {
        return ptr::null_mut();
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.allocate_string_builder_in_region(ListRegion::Persistent)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_builder_new_temporary(
    context: *mut ExecutionContext,
) -> *mut AsterStringBuilder {
    if context.is_null() {
        return ptr::null_mut();
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.allocate_string_builder_in_region(ListRegion::Temporary)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_builder_append(
    context: *mut ExecutionContext,
    builder: *mut AsterStringBuilder,
    value: *const AsterStrHeader,
) {
    if context.is_null() {
        return;
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.string_builder_append(builder, value);
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_builder_to_string(
    context: *mut ExecutionContext,
    builder: *const AsterStringBuilder,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.string_builder_to_string(builder, false)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_string_builder_to_string_temporary(
    context: *mut ExecutionContext,
    builder: *const AsterStringBuilder,
) -> *const AsterStrHeader {
    if context.is_null() {
        return ptr::null();
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.string_builder_to_string(builder, true)
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_list_length(
    context: *mut ExecutionContext,
    list: *const AsterList,
) -> i32 {
    if context.is_null() || list.is_null() {
        return 0;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if !context.validate_list_header(list) {
        return 0;
    }
    // SAFETY: list headers are owned by the live context passed alongside it,
    // and the header was just validated above.
    #[allow(unsafe_code)]
    unsafe {
        (*list).length
    }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_dictionary_new(
    context: *mut ExecutionContext,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
) -> *mut AsterDictionary {
    if context.is_null() {
        return ptr::null_mut();
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(key_kind) = DictionaryKeyKind::from_abi(key_kind) else {
        context.fail("Dictionary allocation received an invalid key kind");
        return ptr::null_mut();
    };
    #[allow(clippy::cast_sign_loss)]
    let key_type_key = key_type_key as u64;
    #[allow(clippy::cast_sign_loss)]
    let value_type_key = value_type_key as u64;
    context.allocate_dictionary_in_region(
        key_kind,
        u32::try_from(key_size).unwrap_or(0),
        u32::try_from(key_align).unwrap_or(0),
        key_type_key,
        u32::try_from(value_size).unwrap_or(0),
        u32::try_from(value_align).unwrap_or(0),
        value_type_key,
        ListRegion::Persistent,
    )
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_dictionary_new_temporary(
    context: *mut ExecutionContext,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
) -> *mut AsterDictionary {
    if context.is_null() {
        return ptr::null_mut();
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(key_kind) = DictionaryKeyKind::from_abi(key_kind) else {
        context.fail("Dictionary allocation received an invalid key kind");
        return ptr::null_mut();
    };
    #[allow(clippy::cast_sign_loss)]
    let key_type_key = key_type_key as u64;
    #[allow(clippy::cast_sign_loss)]
    let value_type_key = value_type_key as u64;
    context.allocate_dictionary_in_region(
        key_kind,
        u32::try_from(key_size).unwrap_or(0),
        u32::try_from(key_align).unwrap_or(0),
        key_type_key,
        u32::try_from(value_size).unwrap_or(0),
        u32::try_from(value_align).unwrap_or(0),
        value_type_key,
        ListRegion::Temporary,
    )
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_dictionary_length(
    context: *mut ExecutionContext,
    dictionary: *const AsterDictionary,
) -> i32 {
    if context.is_null() {
        return 0;
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if context.error.is_some() {
        return 0;
    }
    if dictionary.is_null() {
        context.fail("Dictionary.Length received a null Dictionary");
        return 0;
    }
    if !context.validate_dictionary_header(dictionary) {
        return 0;
    }
    #[allow(unsafe_code)]
    unsafe {
        (*dictionary).length
    }
}

#[allow(clippy::too_many_arguments, clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_dictionary_add(
    context: *mut ExecutionContext,
    dictionary: *mut AsterDictionary,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
    key: *const u8,
    value: *const u8,
) -> i8 {
    dictionary_add_or_set_abi(
        context,
        dictionary,
        key_kind,
        key_size,
        key_align,
        key_type_key,
        value_size,
        value_align,
        value_type_key,
        key,
        value,
        false,
    )
}

#[allow(clippy::too_many_arguments, clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_dictionary_set(
    context: *mut ExecutionContext,
    dictionary: *mut AsterDictionary,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
    key: *const u8,
    value: *const u8,
) -> i8 {
    dictionary_add_or_set_abi(
        context,
        dictionary,
        key_kind,
        key_size,
        key_align,
        key_type_key,
        value_size,
        value_align,
        value_type_key,
        key,
        value,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn dictionary_add_or_set_abi(
    context: *mut ExecutionContext,
    dictionary: *mut AsterDictionary,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
    key: *const u8,
    value: *const u8,
    replace: bool,
) -> i8 {
    if context.is_null() {
        return 0;
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(key_kind) = DictionaryKeyKind::from_abi(key_kind) else {
        context.fail("Dictionary operation received an invalid key kind");
        return 0;
    };
    #[allow(clippy::cast_sign_loss)]
    context.dictionary_add_or_set(
        dictionary,
        key_kind,
        u32::try_from(key_size).unwrap_or(0),
        u32::try_from(key_align).unwrap_or(0),
        key_type_key as u64,
        u32::try_from(value_size).unwrap_or(0),
        u32::try_from(value_align).unwrap_or(0),
        value_type_key as u64,
        key,
        value,
        replace,
    )
}

#[allow(clippy::too_many_arguments, clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_dictionary_contains_key(
    context: *mut ExecutionContext,
    dictionary: *mut AsterDictionary,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
    key: *const u8,
) -> i8 {
    dictionary_contains_or_remove_abi(
        context,
        dictionary,
        key_kind,
        key_size,
        key_align,
        key_type_key,
        value_size,
        value_align,
        value_type_key,
        key,
        false,
    )
}

#[allow(clippy::too_many_arguments, clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_dictionary_remove(
    context: *mut ExecutionContext,
    dictionary: *mut AsterDictionary,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
    key: *const u8,
) -> i8 {
    dictionary_contains_or_remove_abi(
        context,
        dictionary,
        key_kind,
        key_size,
        key_align,
        key_type_key,
        value_size,
        value_align,
        value_type_key,
        key,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn dictionary_contains_or_remove_abi(
    context: *mut ExecutionContext,
    dictionary: *mut AsterDictionary,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
    key: *const u8,
    remove: bool,
) -> i8 {
    if context.is_null() {
        return 0;
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(key_kind) = DictionaryKeyKind::from_abi(key_kind) else {
        context.fail("Dictionary operation received an invalid key kind");
        return 0;
    };
    #[allow(clippy::cast_sign_loss)]
    context.dictionary_contains_or_remove(
        dictionary,
        key_kind,
        u32::try_from(key_size).unwrap_or(0),
        u32::try_from(key_align).unwrap_or(0),
        key_type_key as u64,
        u32::try_from(value_size).unwrap_or(0),
        u32::try_from(value_align).unwrap_or(0),
        value_type_key as u64,
        key,
        remove,
    )
}

#[allow(clippy::too_many_arguments, clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_dictionary_try_get(
    context: *mut ExecutionContext,
    dictionary: *mut AsterDictionary,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
    key: *const u8,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    if context.is_null() {
        return;
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(key_kind) = DictionaryKeyKind::from_abi(key_kind) else {
        context.fail("Dictionary.TryGet received an invalid key kind");
        return;
    };
    #[allow(clippy::cast_sign_loss)]
    context.dictionary_try_get(
        dictionary,
        key_kind,
        u32::try_from(key_size).unwrap_or(0),
        u32::try_from(key_align).unwrap_or(0),
        key_type_key as u64,
        u32::try_from(value_size).unwrap_or(0),
        u32::try_from(value_align).unwrap_or(0),
        value_type_key as u64,
        key,
        destination,
        u32::try_from(total_size).unwrap_or(0),
        u32::try_from(some_tag).unwrap_or(0),
        u32::try_from(none_tag).unwrap_or(0),
        u32::try_from(payload_offset).unwrap_or(u32::MAX),
    );
}

#[allow(clippy::too_many_arguments, clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_dictionary_entries(
    context: *mut ExecutionContext,
    dictionary: *mut AsterDictionary,
    key_kind: i32,
    key_size: i32,
    key_align: i32,
    key_type_key: i64,
    value_size: i32,
    value_align: i32,
    value_type_key: i64,
    entry_size: i32,
    key_offset: i32,
    value_offset: i32,
    temporary: i8,
) -> *mut AsterArray {
    if context.is_null() {
        return ptr::null_mut();
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(key_kind) = DictionaryKeyKind::from_abi(key_kind) else {
        context.fail("Dictionary.Entries received an invalid key kind");
        return ptr::null_mut();
    };
    #[allow(clippy::cast_sign_loss)]
    context.dictionary_entries(
        dictionary,
        key_kind,
        u32::try_from(key_size).unwrap_or(0),
        u32::try_from(key_align).unwrap_or(0),
        key_type_key as u64,
        u32::try_from(value_size).unwrap_or(0),
        u32::try_from(value_align).unwrap_or(0),
        value_type_key as u64,
        u32::try_from(entry_size).unwrap_or(0),
        u32::try_from(key_offset).unwrap_or(u32::MAX),
        u32::try_from(value_offset).unwrap_or(u32::MAX),
        if temporary == 0 {
            ListRegion::Persistent
        } else {
            ListRegion::Temporary
        },
    )
}

/// Reads the current structural-modification counter. See `foreach`'s
/// fail-fast lowering: captured once before the loop, then compared before
/// every element read. Never exposed as an Aster-level property.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_list_version(
    context: *mut ExecutionContext,
    list: *const AsterList,
) -> i64 {
    if context.is_null() || list.is_null() {
        return 0;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if !context.validate_list_header(list) {
        return 0;
    }
    // SAFETY: list headers are owned by the live context passed alongside it,
    // and the header was just validated above.
    #[allow(unsafe_code)]
    let version = unsafe { (*list).version };
    // Bit-preserving reinterpretation, exactly like `element_type_key`'s wire
    // handling: the value is never used arithmetically outside this crate.
    #[allow(clippy::cast_possible_wrap)]
    let version = version as i64;
    version
}

/// Reports the controlled runtime failure for `foreach` detecting `List<T>`
/// structural modification during iteration. Takes no list argument: the
/// message is fixed and does not depend on which list diverged.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_list_version_mismatch(context: *mut ExecutionContext) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.fail("list was structurally modified during foreach iteration");
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_list_add(
    context: *mut ExecutionContext,
    list: *mut AsterList,
    expected_element_size: i32,
    expected_element_align: i32,
    expected_element_type_key: i64,
    source_value_address: *const u8,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let size = u32::try_from(expected_element_size).unwrap_or(0);
    let align = u32::try_from(expected_element_align).unwrap_or(0);
    // `as u64` is a bit-preserving reinterpretation, exactly like
    // `aster_rt_list_new`'s `element_type_key` handling.
    #[allow(clippy::cast_sign_loss)]
    let type_key = expected_element_type_key as u64;
    context.list_add(list, size, align, type_key, source_value_address);
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_list_get(
    context: *mut ExecutionContext,
    list: *const AsterList,
    expected_element_size: i32,
    expected_element_align: i32,
    expected_element_type_key: i64,
    index: i32,
    destination_address: *mut u8,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let size = u32::try_from(expected_element_size).unwrap_or(0);
    let align = u32::try_from(expected_element_align).unwrap_or(0);
    #[allow(clippy::cast_sign_loss)]
    let type_key = expected_element_type_key as u64;
    context.list_get(list, size, align, type_key, index, destination_address);
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_list_remove_at(
    context: *mut ExecutionContext,
    list: *mut AsterList,
    expected_element_size: i32,
    expected_element_align: i32,
    expected_element_type_key: i64,
    index: i32,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated functions receive the live host-owned context as their
    // hidden first parameter, and invocation cannot outlive that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let size = u32::try_from(expected_element_size).unwrap_or(0);
    let align = u32::try_from(expected_element_align).unwrap_or(0);
    #[allow(clippy::cast_sign_loss)]
    let type_key = expected_element_type_key as u64;
    context.list_remove_at(list, size, align, type_key, index);
}

#[cfg(test)]
#[allow(
    clippy::cast_ptr_alignment,
    clippy::ptr_as_ptr,
    clippy::ptr_cast_constness
)]
mod tests {
    use super::*;
    use crate::object::{aster_rt_object_new, aster_rt_object_new_temporary};
    use std::collections::BTreeMap;
    use std::time::Instant;

    #[test]
    fn execution_budget_rejects_large_requests_without_publishing_storage() {
        let mut limited = ExecutionContext::with_allocation_limit(64 * 1024);
        let array = limited.allocate_array(20_000, 4);
        assert!(array.is_null());
        assert_eq!(limited.arena.metrics().reserved_bytes, 0);
        assert!(
            limited
                .take_error()
                .is_some_and(|error| error.contains("execution memory limit"))
        );

        let mut independent = ExecutionContext::new();
        let ordinary = independent.allocate_array(1_000_000, 4);
        assert!(!ordinary.is_null());
        assert!(independent.take_error().is_none());
    }

    #[test]
    fn task_domain_main_limit_preserves_retained_capacity_and_freezes_future_growth() {
        let governor = Arc::new(MemoryGovernor::new(64 * 1024));
        let mut context = ExecutionContext::with_memory_governor(Arc::clone(&governor));
        let unrelated = Arc::new(MemoryGovernor::new(64 * 1024));
        assert!(context.is_governed_by(&governor));
        assert!(!context.is_governed_by(&unrelated));
        assert!(!ExecutionContext::new().is_governed_by(&governor));
        assert!(!context.allocate_array(1, 1).is_null());
        let retained = context
            .retained_arena_capacity_bytes()
            .expect("retained capacity is addressable");
        assert_eq!(retained, ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES);
        assert_eq!(context.freeze_task_domain_main_limit(0), Some(retained));

        assert!(context.allocate_array(2_000, 4).is_null());
        assert_eq!(context.retained_arena_capacity_bytes(), Some(retained));
        assert_eq!(governor.telemetry().grant_events, 1);
        assert!(context.take_error().is_some_and(|error| {
            error.contains("deterministic Task.Run main-context memory entitlement")
        }));
    }

    #[test]
    fn task_entitlement_denial_precedes_shared_governor_admission() {
        let governor = Arc::new(MemoryGovernor::new(64 * 1024));
        let mut context = ExecutionContext::with_task_memory_budget(
            Arc::clone(&governor),
            ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES - 1,
        );

        assert!(context.allocate_array(1, 1).is_null());
        assert_eq!(context.retained_arena_capacity_bytes(), Some(0));
        assert_eq!(governor.telemetry().grant_events, 0);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("deterministic Task.Run memory entitlement"))
        );
    }

    #[test]
    fn async_entitlement_denials_precede_shared_governor_admission() {
        let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES;
        let governor = Arc::new(MemoryGovernor::new(2 * page));
        let mut move_next =
            ExecutionContext::with_async_move_next_memory_budget(Arc::clone(&governor), page - 1);
        assert!(move_next.allocate_array(1, 1).is_null());
        assert!(move_next.take_error().is_some_and(|error| {
            error.contains("deterministic async MoveNext memory entitlement")
        }));

        let mut inner =
            ExecutionContext::with_async_inner_memory_budget(Arc::clone(&governor), page - 1);
        assert!(inner.allocate_array(1, 1).is_null());
        assert!(inner.take_error().is_some_and(|error| {
            error.contains("deterministic async awaited-inner memory entitlement")
        }));
        assert_eq!(governor.telemetry().grant_events, 0);
    }

    #[test]
    fn async_main_limit_preserves_retained_capacity() {
        let governor = Arc::new(MemoryGovernor::new(64 * 1024));
        let mut context = ExecutionContext::with_memory_governor(Arc::clone(&governor));
        assert!(!context.allocate_array(1, 1).is_null());
        let retained = context.retained_arena_capacity_bytes().unwrap();
        assert_eq!(context.freeze_async_domain_main_limit(0), Some(retained));
        assert!(context.allocate_array(2_000, 4).is_null());
        assert_eq!(context.retained_arena_capacity_bytes(), Some(retained));
        assert!(context.take_error().is_some_and(|error| {
            error.contains("deterministic async Main memory entitlement")
        }));
    }

    #[test]
    fn shared_governor_remains_final_authority_for_async_contexts() {
        let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES;
        let governor = Arc::new(MemoryGovernor::new(page));
        let mut move_next =
            ExecutionContext::with_async_move_next_memory_budget(Arc::clone(&governor), 2 * page);
        let mut inner =
            ExecutionContext::with_async_inner_memory_budget(Arc::clone(&governor), 2 * page);
        assert!(!move_next.allocate_array(1, 1).is_null());
        assert!(inner.allocate_array(1, 1).is_null());
        assert!(
            inner
                .take_error()
                .is_some_and(|error| error.contains("shared execution memory budget"))
        );
        let telemetry = governor.telemetry();
        assert_eq!(telemetry.current_capacity_bytes, page as u64);
        assert_eq!(telemetry.denial_events, 1);
        drop(inner);
        drop(move_next);
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }

    #[test]
    fn shared_governor_remains_final_authority_for_task_contexts() {
        let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES;
        let governor = Arc::new(MemoryGovernor::new(page));
        let mut first = ExecutionContext::with_task_memory_budget(Arc::clone(&governor), 2 * page);
        let mut second = ExecutionContext::with_task_memory_budget(Arc::clone(&governor), 2 * page);

        assert!(!first.allocate_array(1, 1).is_null());
        assert!(second.allocate_array(1, 1).is_null());
        assert!(
            second
                .take_error()
                .is_some_and(|error| error.contains("shared execution memory budget"))
        );
        let telemetry = governor.telemetry();
        let page = u64::try_from(page).expect("page capacity fits u64");
        assert_eq!(telemetry.current_capacity_bytes, page);
        assert_eq!(telemetry.peak_capacity_bytes, page);
        assert_eq!(telemetry.grant_events, 1);
        assert_eq!(telemetry.denial_events, 1);
    }

    #[test]
    fn default_budget_preserves_the_verified_four_hundred_megabyte_array() {
        let payload = 50_000_000_usize
            .checked_mul(size_of::<i64>())
            .expect("verified array payload is addressable");
        // The dedicated data page is exactly `payload` bytes; the following
        // array header requires one ordinary arena page because that data page
        // has no remaining capacity.
        let required_reservation = payload
            .checked_add(DEFAULT_PAGE_SIZE)
            .expect("verified array reservation is addressable");

        assert!(required_reservation > 256 * 1024 * 1024);
        assert!(required_reservation <= EXECUTION_ALLOCATION_LIMIT_BYTES);
    }

    #[test]
    fn allocation_size_multiplication_is_checked() {
        assert_eq!(checked_allocation_size(1024, 4), Some(4096));
        assert_eq!(checked_allocation_size(usize::MAX, 2), None);
    }

    #[test]
    fn list_growth_uses_the_same_execution_budget() {
        let mut context = ExecutionContext::with_allocation_limit(64 * 1024);
        let list = context.allocate_list_in_region(64 * 1024, 8, 0x1234, ListRegion::Persistent);
        assert!(!list.is_null());
        let source = [0_u8; 1];
        context.list_add(list, 64 * 1024, 8, 0x1234, source.as_ptr());
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("execution memory limit"))
        );
        // SAFETY: `list` is a valid header and failed growth is atomic.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*list).length, 0);
            assert_eq!((*list).capacity, 0);
            assert!((*list).data.is_null());
        }
    }

    #[test]
    fn empty_list_header_satisfies_every_invariant() {
        let mut context = ExecutionContext::new();
        let header = context.allocate_list_in_region(4, 4, 0xdead_beef, ListRegion::Persistent);
        assert!(!header.is_null());
        // SAFETY: `header` was just allocated above and is not aliased.
        #[allow(unsafe_code)]
        let list = unsafe { &*header };
        assert_eq!(list.length(), 0);
        assert_eq!(list.capacity(), 0);
        assert_eq!(list.element_size(), 4);
        assert_eq!(list.element_align(), 4);
        assert_eq!(list.element_type_key(), 0xdead_beef);
        assert_eq!(list.region(), ListRegion::Persistent);
        assert!(context.take_error().is_none());
    }

    #[test]
    fn list_rejects_zero_element_size() {
        let mut context = ExecutionContext::new();
        let header = context.allocate_list_in_region(0, 4, 1, ListRegion::Persistent);
        assert!(header.is_null());
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("size"))
        );
    }

    #[test]
    fn empty_dictionary_header_is_valid_and_allocates_no_buffers() {
        let mut context = ExecutionContext::new();
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Long,
            8,
            8,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        assert!(!dictionary.is_null());
        assert_eq!(aster_rt_dictionary_length(&raw mut context, dictionary), 0);
        // SAFETY: this test owns the fresh runtime header and only observes it.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*dictionary).bucket_capacity, 0);
            assert_eq!((*dictionary).entry_capacity, 0);
            assert_eq!((*dictionary).entry_count, 0);
            assert_eq!((*dictionary).region(), ListRegion::Persistent);
        }
        assert!(context.take_error().is_none());
    }

    #[test]
    fn dictionary_rejects_invalid_layout_without_publishing_a_header() {
        let mut context = ExecutionContext::new();
        assert!(
            context
                .allocate_dictionary_in_region(
                    DictionaryKeyKind::Int,
                    4,
                    3,
                    1,
                    4,
                    4,
                    2,
                    ListRegion::Persistent,
                )
                .is_null()
        );
        assert!(context.take_error().is_some());
    }

    #[test]
    fn dictionary_failures_are_first_error_wins_and_context_local() {
        let mut failing = ExecutionContext::new();
        assert!(
            failing
                .allocate_dictionary_in_region(
                    DictionaryKeyKind::Int,
                    4,
                    3,
                    1,
                    4,
                    4,
                    2,
                    ListRegion::Persistent,
                )
                .is_null()
        );
        let first = failing
            .take_error()
            .expect("invalid alignment records an error");
        // Restore the first error through a second controlled failure: the context's
        // normal first-error policy must not be replaced by later operations.
        failing.fail(first.clone());
        assert!(
            failing
                .allocate_dictionary_in_region(
                    DictionaryKeyKind::Int,
                    0,
                    4,
                    1,
                    4,
                    4,
                    2,
                    ListRegion::Persistent,
                )
                .is_null()
        );
        assert_eq!(failing.take_error(), Some(first));

        let mut independent = ExecutionContext::new();
        let dictionary = independent.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            8,
            8,
            2,
            ListRegion::Persistent,
        );
        assert!(!dictionary.is_null());
        assert_eq!(
            aster_rt_dictionary_length(&raw mut independent, dictionary),
            0
        );
        assert!(independent.take_error().is_none());
    }

    #[test]
    fn dictionary_owner_and_corrupted_chain_fail_without_cross_context_contamination() {
        let mut owner = ExecutionContext::new();
        let dictionary = owner.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        let key = 7_i32;
        let value = 42_i32;
        assert_eq!(
            owner.dictionary_add_or_set(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const key).cast(),
                (&raw const value).cast(),
                false,
            ),
            1
        );

        let mut wrong_context = ExecutionContext::new();
        assert_eq!(
            aster_rt_dictionary_length(&raw mut wrong_context, dictionary),
            0
        );
        let ownership_error = wrong_context.take_error().expect("ownership error");
        assert!(
            ownership_error.contains("ExecutionContext"),
            "{ownership_error}"
        );
        assert_eq!(aster_rt_dictionary_length(&raw mut owner, dictionary), 1);
        assert!(owner.take_error().is_none());

        // Corrupt only the bucket index in a valid, owned allocation. No
        // arbitrary pointer is fabricated or dereferenced.
        let hash = owner
            .dictionary_hash_key(DictionaryKeyKind::Int, (&raw const key).cast(), 4, "test")
            .expect("hash");
        // SAFETY: the header and bucket buffer were allocated by `owner`.
        #[allow(unsafe_code)]
        unsafe {
            let bucket = usize::try_from(
                hash & (u64::try_from((*dictionary).bucket_capacity).expect("capacity") - 1),
            )
            .expect("bucket");
            *(*dictionary).buckets.add(bucket) =
                u32::try_from((*dictionary).entry_count).expect("entry count");
        }
        assert_eq!(
            owner.dictionary_contains_or_remove(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const key).cast(),
                false,
            ),
            0
        );
        let chain_error = owner.take_error().expect("chain error");
        assert!(
            chain_error.contains("invalid") || chain_error.contains("cyclic"),
            "{chain_error}"
        );

        let mut independent = ExecutionContext::new();
        let valid = independent.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        assert_eq!(aster_rt_dictionary_length(&raw mut independent, valid), 0);
        assert!(independent.take_error().is_none());
    }

    #[test]
    fn dictionary_first_error_prevents_later_publication_or_replacement() {
        let mut context = ExecutionContext::new();
        context.fail("first dictionary failure");
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        assert!(dictionary.is_null());
        context.fail("later dictionary failure");
        assert_eq!(
            context.take_error().as_deref(),
            Some("first dictionary failure")
        );
    }

    #[test]
    fn siphash13_matches_reference_vectors() {
        let key0 = 0x0706_0504_0302_0100;
        let key1 = 0x0f0e_0d0c_0b0a_0908;
        let input = (0_u8..64).collect::<Vec<_>>();
        let expected = [
            0xabac_0158_050f_c4dc,
            0xc9f4_9bf3_7d57_ca93,
            0x82cb_9b02_4dc7_d44d,
            0x8bf8_0ab8_e7dd_f7fb,
            0xcf75_5760_88d3_8328,
        ];
        for (length, expected) in expected.into_iter().enumerate() {
            assert_eq!(siphash13(key0, key1, &input[..length]), expected);
        }
    }

    #[test]
    fn dictionary_hashes_every_key_kind_from_its_canonical_bytes() {
        let mut context = ExecutionContext::new();
        context.dictionary_hash_k0 = 0x0706_0504_0302_0100;
        context.dictionary_hash_k1 = 0x0f0e_0d0c_0b0a_0908;
        macro_rules! check {
            ($kind:expr, $value:expr, $bytes:expr) => {{
                let value = $value;
                let bytes = $bytes;
                assert_eq!(
                    context
                        .dictionary_hash_key(
                            $kind,
                            (&raw const value).cast(),
                            u32::try_from(bytes.len()).expect("key size"),
                            "test",
                        )
                        .expect("hash"),
                    siphash13(
                        context.dictionary_hash_k0,
                        context.dictionary_hash_k1,
                        &bytes,
                    )
                );
            }};
        }
        check!(DictionaryKeyKind::Bool, 1_u8, [1_u8]);
        check!(
            DictionaryKeyKind::Char,
            u32::from('\u{1f642}'),
            u32::from('\u{1f642}').to_le_bytes()
        );
        check!(DictionaryKeyKind::SByte, i8::MIN, i8::MIN.to_le_bytes());
        check!(DictionaryKeyKind::Byte, u8::MAX, u8::MAX.to_le_bytes());
        check!(DictionaryKeyKind::Short, i16::MIN, i16::MIN.to_le_bytes());
        check!(DictionaryKeyKind::UShort, u16::MAX, u16::MAX.to_le_bytes());
        check!(DictionaryKeyKind::Int, i32::MIN, i32::MIN.to_le_bytes());
        check!(DictionaryKeyKind::UInt, u32::MAX, u32::MAX.to_le_bytes());
        check!(DictionaryKeyKind::Long, i64::MIN, i64::MIN.to_le_bytes());
        check!(DictionaryKeyKind::ULong, u64::MAX, u64::MAX.to_le_bytes());

        let string = context.allocate_string_parts(&["a\0\u{00e9}"]);
        assert_eq!(
            context
                .dictionary_hash_key(
                    DictionaryKeyKind::String,
                    (&raw const string).cast(),
                    u32::try_from(size_of::<*const AsterStrHeader>()).expect("pointer"),
                    "test",
                )
                .expect("string hash"),
            siphash13(
                context.dictionary_hash_k0,
                context.dictionary_hash_k1,
                "a\0\u{00e9}".as_bytes(),
            )
        );
        assert!(context.take_error().is_none());
    }

    #[test]
    fn dictionary_public_order_does_not_depend_on_context_seed() {
        fn build(k0: u64, k1: u64) -> Vec<(i32, i32)> {
            let mut context = ExecutionContext::new();
            context.dictionary_hash_k0 = k0;
            context.dictionary_hash_k1 = k1;
            let dictionary = context.allocate_dictionary_in_region(
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                ListRegion::Persistent,
            );
            for key in [9_i32, 1, 7, 3, 5] {
                let value = key * 10;
                assert_eq!(
                    context.dictionary_add_or_set(
                        dictionary,
                        DictionaryKeyKind::Int,
                        4,
                        4,
                        1,
                        4,
                        4,
                        2,
                        (&raw const key).cast(),
                        (&raw const value).cast(),
                        false,
                    ),
                    1
                );
            }
            live_int_entries(dictionary)
        }
        assert_eq!(build(1, 2), build(0xfeed, 0xbeef));
    }

    #[test]
    fn dictionary_collision_chain_supports_head_middle_and_tail_removal() {
        let mut context = ExecutionContext::new();
        context.dictionary_hash_k0 = 1;
        context.dictionary_hash_k1 = 2;
        let mut colliding = Vec::new();
        for candidate in 0_i32..10_000 {
            let hash = context
                .dictionary_hash_key(
                    DictionaryKeyKind::Int,
                    (&raw const candidate).cast(),
                    4,
                    "test",
                )
                .expect("hash");
            if hash.trailing_zeros() >= 3 {
                colliding.push(candidate);
                if colliding.len() == 4 {
                    break;
                }
            }
        }
        assert_eq!(colliding.len(), 4);
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        for key in &colliding {
            assert_eq!(
                context.dictionary_add_or_set(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    ptr::from_ref(key).cast(),
                    ptr::from_ref(key).cast(),
                    false,
                ),
                1
            );
        }
        for index in [3_usize, 1, 0] {
            let key = colliding[index];
            assert_eq!(
                context.dictionary_contains_or_remove(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    true,
                ),
                1
            );
        }
        let remaining = colliding[2];
        assert_eq!(
            context.dictionary_contains_or_remove(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const remaining).cast(),
                false,
            ),
            1
        );
        assert_eq!(live_int_entries(dictionary), vec![(remaining, remaining)]);
        assert!(context.take_error().is_none());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn dictionary_growth_tombstones_and_snapshot_preserve_insertion_order() {
        let mut context = ExecutionContext::with_stats();
        context.dictionary_hash_k0 = 0x0706_0504_0302_0100;
        context.dictionary_hash_k1 = 0x0f0e_0d0c_0b0a_0908;
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        assert!(!dictionary.is_null());
        for key in 0_i32..1_000 {
            let value = key * 2;
            assert_eq!(
                context.dictionary_add_or_set(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    (&raw const value).cast(),
                    false,
                ),
                1
            );
        }
        for key in (0_i32..1_000).step_by(2) {
            assert_eq!(
                context.dictionary_contains_or_remove(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    true,
                ),
                1
            );
        }
        for key in (0_i32..1_000).step_by(2) {
            let value = key * 3;
            assert_eq!(
                context.dictionary_add_or_set(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    (&raw const value).cast(),
                    false,
                ),
                1
            );
        }
        let before_snapshot = context.memory_stats().clone();
        let snapshot = context.dictionary_entries(
            dictionary,
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            8,
            0,
            4,
            ListRegion::Persistent,
        );
        assert!(!snapshot.is_null());
        assert_eq!(
            context.memory_stats().total_allocations - before_snapshot.total_allocations,
            1
        );
        assert_eq!(
            context.memory_stats().array_allocations - before_snapshot.array_allocations,
            1
        );
        assert_eq!(
            context.memory_stats().string_allocations,
            before_snapshot.string_allocations
        );
        // SAFETY: the runtime allocated a 1000-element array of two i32 fields.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*snapshot).length, 1_000);
            let pairs = std::slice::from_raw_parts((*snapshot).data.cast::<i32>(), 2_000);
            assert_eq!(&pairs[..4], &[1, 2, 3, 6]);
            assert_eq!(&pairs[998..1002], &[999, 1998, 0, 0]);
            assert_eq!(&pairs[1998..], &[998, 2994]);
            assert_eq!((*dictionary).bucket_capacity.count_ones(), 1);
            assert_eq!((*dictionary).length, 1_000);
        }
        assert!(context.take_error().is_none());
    }

    #[test]
    fn sparse_tombstone_churn_grows_once_instead_of_rebuilding_repeatedly() {
        let mut context = ExecutionContext::with_stats();
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        for key in 0_i32..1_000 {
            assert_eq!(
                context.dictionary_add_or_set(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    (&raw const key).cast(),
                    false,
                ),
                1
            );
        }
        let before = context.memory_stats().total_allocations;
        for key in 0_i32..1_000 {
            assert_eq!(
                context.dictionary_contains_or_remove(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    true,
                ),
                1
            );
            assert_eq!(
                context.dictionary_add_or_set(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    (&raw const key).cast(),
                    false,
                ),
                1
            );
        }

        assert_eq!(context.memory_stats().total_allocations - before, 2);
        assert_eq!(
            live_int_entries(dictionary),
            (0_i32..1_000).map(|key| (key, key)).collect::<Vec<_>>()
        );
        assert!(context.take_error().is_none());
    }

    #[test]
    fn dictionary_read_operations_and_duplicates_do_not_allocate() {
        let mut context = ExecutionContext::with_stats();
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        let key = 7_i32;
        let value = 42_i32;
        assert_eq!(
            context.dictionary_add_or_set(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const key).cast(),
                (&raw const value).cast(),
                false,
            ),
            1
        );
        let before = context.memory_stats().clone();
        for _ in 0..1_000 {
            assert_eq!(aster_rt_dictionary_length(&raw mut context, dictionary), 1);
            assert_eq!(
                context.dictionary_contains_or_remove(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    false,
                ),
                1
            );
            assert_eq!(
                context.dictionary_add_or_set(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    (&raw const value).cast(),
                    false,
                ),
                0
            );
            let mut option = [0_u8; 8];
            context.dictionary_try_get(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const key).cast(),
                option.as_mut_ptr(),
                8,
                1,
                0,
                4,
            );
            assert_eq!(u32::from_ne_bytes(option[..4].try_into().expect("tag")), 1);
            assert_eq!(
                i32::from_ne_bytes(option[4..].try_into().expect("value")),
                42
            );
        }
        assert_eq!(context.memory_stats(), &before);
        assert!(context.take_error().is_none());
    }

    fn live_int_entries(dictionary: *const AsterDictionary) -> Vec<(i32, i32)> {
        // SAFETY: callers pass a runtime-created Dictionary<int, int> whose
        // header remains owned by the live test context.
        #[allow(unsafe_code)]
        unsafe {
            let layout = dictionary_entry_layout(4, 4, 4, 4).expect("i32 entry layout");
            let mut entries = Vec::new();
            for index in 0..(*dictionary).entry_count {
                let entry = ExecutionContext::dictionary_entry_pointer(
                    (*dictionary).entries,
                    index,
                    layout,
                )
                .expect("validated entry");
                if ExecutionContext::dictionary_entry_live(entry) == 1 {
                    entries.push((
                        ptr::read_unaligned(entry.add(layout.key_offset).cast::<i32>()),
                        ptr::read_unaligned(entry.add(layout.value_offset).cast::<i32>()),
                    ));
                }
            }
            entries
        }
    }

    fn live_string_entries(dictionary: *const AsterDictionary) -> Vec<(String, i32)> {
        // SAFETY: callers pass a runtime-created Dictionary<string, int>.
        // Every string pointer was allocated persistently by the same live
        // context before insertion.
        #[allow(unsafe_code)]
        unsafe {
            let pointer_size = u32::try_from(size_of::<*const AsterStrHeader>()).expect("pointer");
            let pointer_align =
                u32::try_from(align_of::<*const AsterStrHeader>()).expect("pointer");
            let layout =
                dictionary_entry_layout(pointer_size, pointer_align, 4, 4).expect("string entry");
            let mut entries = Vec::new();
            for index in 0..(*dictionary).entry_count {
                let entry = ExecutionContext::dictionary_entry_pointer(
                    (*dictionary).entries,
                    index,
                    layout,
                )
                .expect("validated entry");
                if ExecutionContext::dictionary_entry_live(entry) == 1 {
                    let key = ptr::read_unaligned(
                        entry.add(layout.key_offset).cast::<*const AsterStrHeader>(),
                    );
                    entries.push((
                        crate::string::view(key)
                            .expect("valid stored string")
                            .to_owned(),
                        ptr::read_unaligned(entry.add(layout.value_offset).cast::<i32>()),
                    ));
                }
            }
            entries
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn dictionary_differential_int_model_matches_every_operation_and_order() {
        let mut context = ExecutionContext::with_stats();
        context.dictionary_hash_k0 = 0x0706_0504_0302_0100;
        context.dictionary_hash_k1 = 0x0f0e_0d0c_0b0a_0908;
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        let mut values = BTreeMap::<i32, i32>::new();
        let mut order = Vec::<i32>::new();
        let mut state = 0x4d34_2d69_6e74_u64;
        for step in 0_i32..5_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let key = i32::try_from(state % 97).expect("small key") - 48;
            let value = step.wrapping_mul(31).wrapping_add(key);
            match (state >> 32) % 6 {
                0 => {
                    let expected = !values.contains_key(&key);
                    let actual = context.dictionary_add_or_set(
                        dictionary,
                        DictionaryKeyKind::Int,
                        4,
                        4,
                        1,
                        4,
                        4,
                        2,
                        (&raw const key).cast(),
                        (&raw const value).cast(),
                        false,
                    ) != 0;
                    assert_eq!(actual, expected);
                    if expected {
                        values.insert(key, value);
                        order.push(key);
                    }
                }
                1 => {
                    let expected = values.contains_key(&key);
                    let actual = context.dictionary_add_or_set(
                        dictionary,
                        DictionaryKeyKind::Int,
                        4,
                        4,
                        1,
                        4,
                        4,
                        2,
                        (&raw const key).cast(),
                        (&raw const value).cast(),
                        true,
                    ) != 0;
                    assert_eq!(actual, expected);
                    if !expected {
                        order.push(key);
                    }
                    values.insert(key, value);
                }
                2 => {
                    let actual = context.dictionary_contains_or_remove(
                        dictionary,
                        DictionaryKeyKind::Int,
                        4,
                        4,
                        1,
                        4,
                        4,
                        2,
                        (&raw const key).cast(),
                        false,
                    ) != 0;
                    assert_eq!(actual, values.contains_key(&key));
                }
                3 => {
                    let expected = values.remove(&key).is_some();
                    let actual = context.dictionary_contains_or_remove(
                        dictionary,
                        DictionaryKeyKind::Int,
                        4,
                        4,
                        1,
                        4,
                        4,
                        2,
                        (&raw const key).cast(),
                        true,
                    ) != 0;
                    assert_eq!(actual, expected);
                    if expected {
                        order.retain(|existing| *existing != key);
                    }
                }
                4 => {
                    let mut option = [0_u8; 8];
                    context.dictionary_try_get(
                        dictionary,
                        DictionaryKeyKind::Int,
                        4,
                        4,
                        1,
                        4,
                        4,
                        2,
                        (&raw const key).cast(),
                        option.as_mut_ptr(),
                        8,
                        1,
                        0,
                        4,
                    );
                    let actual = if u32::from_ne_bytes(option[..4].try_into().expect("tag")) == 1 {
                        Some(i32::from_ne_bytes(option[4..].try_into().expect("payload")))
                    } else {
                        None
                    };
                    assert_eq!(actual, values.get(&key).copied());
                }
                _ => {}
            }
            let expected = order
                .iter()
                .map(|key| (*key, values[key]))
                .collect::<Vec<_>>();
            assert_eq!(live_int_entries(dictionary), expected);
            // SAFETY: the header belongs to this live context.
            #[allow(unsafe_code)]
            unsafe {
                assert_eq!(
                    (*dictionary).length,
                    i32::try_from(values.len()).expect("small model")
                );
            }
        }
        assert!(context.take_error().is_none());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn dictionary_differential_string_model_is_ordinal_and_ordered() {
        let mut context = ExecutionContext::with_stats();
        context.dictionary_hash_k0 = 7;
        context.dictionary_hash_k1 = 11;
        let pointer_size = u32::try_from(size_of::<*const AsterStrHeader>()).expect("pointer");
        let pointer_align = u32::try_from(align_of::<*const AsterStrHeader>()).expect("pointer");
        let keys = [
            "",
            "A",
            "a",
            "\u{00e9}",
            "e\u{0301}",
            "a\0b",
            "\u{03b2}",
            "\u{1f642}",
            "word-0",
            "word-1",
            "word-2",
            "word-3",
            "word-4",
        ];
        let pointers = keys
            .iter()
            .map(|key| context.allocate_string_parts(&[key]))
            .collect::<Vec<_>>();
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::String,
            pointer_size,
            pointer_align,
            3,
            4,
            4,
            4,
            ListRegion::Persistent,
        );
        let mut values = BTreeMap::<String, i32>::new();
        let mut order = Vec::<String>::new();
        let mut state = 0x4d34_2d73_7472_u64;
        for step in 0_i32..2_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let key_index = usize::try_from(state % keys.len() as u64).expect("key index");
            let key = keys[key_index];
            let key_pointer = pointers[key_index];
            let value = step.wrapping_mul(17);
            match (state >> 29) % 6 {
                0 => {
                    let expected = !values.contains_key(key);
                    let actual = context.dictionary_add_or_set(
                        dictionary,
                        DictionaryKeyKind::String,
                        pointer_size,
                        pointer_align,
                        3,
                        4,
                        4,
                        4,
                        (&raw const key_pointer).cast(),
                        (&raw const value).cast(),
                        false,
                    ) != 0;
                    assert_eq!(actual, expected);
                    if expected {
                        values.insert(key.to_owned(), value);
                        order.push(key.to_owned());
                    }
                }
                1 => {
                    let expected = values.contains_key(key);
                    let actual = context.dictionary_add_or_set(
                        dictionary,
                        DictionaryKeyKind::String,
                        pointer_size,
                        pointer_align,
                        3,
                        4,
                        4,
                        4,
                        (&raw const key_pointer).cast(),
                        (&raw const value).cast(),
                        true,
                    ) != 0;
                    assert_eq!(actual, expected);
                    if !expected {
                        order.push(key.to_owned());
                    }
                    values.insert(key.to_owned(), value);
                }
                2 => {
                    assert_eq!(
                        context.dictionary_contains_or_remove(
                            dictionary,
                            DictionaryKeyKind::String,
                            pointer_size,
                            pointer_align,
                            3,
                            4,
                            4,
                            4,
                            (&raw const key_pointer).cast(),
                            false,
                        ) != 0,
                        values.contains_key(key)
                    );
                }
                3 => {
                    let expected = values.remove(key).is_some();
                    assert_eq!(
                        context.dictionary_contains_or_remove(
                            dictionary,
                            DictionaryKeyKind::String,
                            pointer_size,
                            pointer_align,
                            3,
                            4,
                            4,
                            4,
                            (&raw const key_pointer).cast(),
                            true,
                        ) != 0,
                        expected
                    );
                    if expected {
                        order.retain(|existing| existing != key);
                    }
                }
                4 => {
                    let mut option = [0_u8; 8];
                    context.dictionary_try_get(
                        dictionary,
                        DictionaryKeyKind::String,
                        pointer_size,
                        pointer_align,
                        3,
                        4,
                        4,
                        4,
                        (&raw const key_pointer).cast(),
                        option.as_mut_ptr(),
                        8,
                        1,
                        0,
                        4,
                    );
                    let actual = if u32::from_ne_bytes(option[..4].try_into().expect("tag")) == 1 {
                        Some(i32::from_ne_bytes(option[4..].try_into().expect("payload")))
                    } else {
                        None
                    };
                    assert_eq!(actual, values.get(key).copied());
                }
                _ => {}
            }
            let expected = order
                .iter()
                .map(|key| (key.clone(), values[key]))
                .collect::<Vec<_>>();
            assert_eq!(live_string_entries(dictionary), expected);
        }
        assert!(context.take_error().is_none());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn dictionary_limits_and_large_operation_phases_are_atomic() {
        let started = Instant::now();
        let mut context = ExecutionContext::with_stats();
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        for key in 0_i32..DICTIONARY_MAX_ENTRIES {
            assert_eq!(
                context.dictionary_add_or_set(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    (&raw const key).cast(),
                    false,
                ),
                1
            );
        }
        for key in 0_i32..DICTIONARY_MAX_ENTRIES {
            assert_eq!(
                context.dictionary_contains_or_remove(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    false,
                ),
                1
            );
        }
        for key in (0_i32..DICTIONARY_MAX_ENTRIES).step_by(2) {
            assert_eq!(
                context.dictionary_contains_or_remove(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    true,
                ),
                1
            );
        }
        for key in (0_i32..DICTIONARY_MAX_ENTRIES).step_by(2) {
            assert_eq!(
                context.dictionary_add_or_set(
                    dictionary,
                    DictionaryKeyKind::Int,
                    4,
                    4,
                    1,
                    4,
                    4,
                    2,
                    (&raw const key).cast(),
                    (&raw const key).cast(),
                    false,
                ),
                1
            );
        }
        let extra = DICTIONARY_MAX_ENTRIES;
        assert_eq!(
            context.dictionary_add_or_set(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const extra).cast(),
                (&raw const extra).cast(),
                false,
            ),
            0
        );
        // SAFETY: the header remains owned by this live context. The failed
        // insertion must not publish a partial entry or change Length.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*dictionary).length, DICTIONARY_MAX_ENTRIES);
        }
        let error = context.take_error().expect("limit failure");
        assert!(error.contains("100000"), "{error}");
        eprintln!(
            "dictionary 100k stress: {:?}; stats={:?}",
            started.elapsed(),
            context.memory_stats()
        );
    }

    #[test]
    fn dictionary_active_byte_limit_fails_before_publishing_buffers() {
        let mut context = ExecutionContext::with_stats();
        let value_size = 8 * 1024 * 1024;
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            value_size,
            1,
            2,
            ListRegion::Persistent,
        );
        assert!(!dictionary.is_null());
        let before = context.memory_stats().clone();
        let key = 1_i32;
        let value = vec![0_u8; usize::try_from(value_size).expect("value size")];
        assert_eq!(
            context.dictionary_add_or_set(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                value_size,
                1,
                2,
                (&raw const key).cast(),
                value.as_ptr(),
                false,
            ),
            0
        );
        // SAFETY: the failed first insertion must leave the controlled header
        // in its original empty state and publish neither buffer.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*dictionary).length, 0);
            assert_eq!((*dictionary).entry_count, 0);
            assert_eq!((*dictionary).entry_capacity, 0);
            assert_eq!((*dictionary).bucket_capacity, 0);
            assert!((*dictionary).entries.is_null());
            assert!((*dictionary).buckets.is_null());
        }
        assert_eq!(context.memory_stats(), &before);
        let error = context.take_error().expect("active byte limit");
        assert!(error.contains("64 MiB"), "{error}");
    }

    #[test]
    fn dictionary_active_byte_limit_accepts_exactly_64_mib_and_rejects_one_stride_more() {
        let layout = DictionaryEntryLayout {
            key_offset: 0,
            value_offset: 0,
            stride: 8,
            align: 8,
        };
        let mut exact = ExecutionContext::new();
        assert_eq!(
            exact.dictionary_active_bytes(0, 8 * 1024 * 1024, layout),
            Some((0, DICTIONARY_MAX_ACTIVE_BYTES))
        );
        assert!(exact.take_error().is_none());

        let mut over = ExecutionContext::new();
        assert!(
            over.dictionary_active_bytes(0, 8 * 1024 * 1024 + 1, layout)
                .is_none()
        );
        assert!(
            over.take_error()
                .is_some_and(|error| error.contains("64 MiB"))
        );
    }

    #[test]
    fn dictionary_buffers_fail_atomically_before_reserving_partial_storage() {
        let mut context = ExecutionContext::with_allocation_limit(8 * 1024 * 1024);
        let before = context.memory_stats().clone();
        let layout = dictionary_entry_layout(4, 4, 128, 8).expect("large value entry layout");

        assert!(
            context
                .dictionary_allocate_buffers(131_072, 65_536, layout, ListRegion::Persistent, 0,)
                .is_none()
        );
        assert_eq!(context.memory_stats(), &before);
        assert_eq!(context.arena.metrics().reserved_bytes, 0);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("execution memory limit"))
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn dictionary_allocation_metrics_match_header_buffers_and_snapshot() {
        let mut context = ExecutionContext::with_stats();
        let initial = context.memory_stats().clone();
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Persistent,
        );
        let after_header = context.memory_stats().clone();
        assert_eq!(
            after_header.total_allocations - initial.total_allocations,
            1
        );
        assert_eq!(
            after_header.object_allocations - initial.object_allocations,
            1
        );
        assert_eq!(after_header.string_allocations, 0);

        let alias = dictionary;
        for _ in 0..1_000 {
            assert_eq!(aster_rt_dictionary_length(&raw mut context, alias), 0);
        }
        assert_eq!(context.memory_stats(), &after_header);

        let key = 1_i32;
        let value = 2_i32;
        assert_eq!(
            context.dictionary_add_or_set(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const key).cast(),
                (&raw const value).cast(),
                false,
            ),
            1
        );
        let after_first_insert = context.memory_stats().clone();
        assert_eq!(
            after_first_insert.total_allocations - after_header.total_allocations,
            2
        );
        assert_eq!(
            after_first_insert.object_allocations - after_header.object_allocations,
            2
        );
        assert_eq!(after_first_insert.string_allocations, 0);

        let second_key = 2_i32;
        let second_value = 3_i32;
        assert_eq!(
            context.dictionary_add_or_set(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const second_key).cast(),
                (&raw const second_value).cast(),
                false,
            ),
            1
        );
        assert_eq!(
            context.dictionary_add_or_set(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const second_key).cast(),
                (&raw const value).cast(),
                true,
            ),
            1
        );
        assert_eq!(
            context.dictionary_contains_or_remove(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const second_key).cast(),
                false,
            ),
            1
        );
        assert_eq!(
            context.dictionary_contains_or_remove(
                dictionary,
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                (&raw const second_key).cast(),
                true,
            ),
            1
        );
        assert_eq!(context.memory_stats(), &after_first_insert);

        let snapshot = context.dictionary_entries(
            dictionary,
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            8,
            0,
            4,
            ListRegion::Persistent,
        );
        assert!(!snapshot.is_null());
        let after_snapshot = context.memory_stats().clone();
        assert_eq!(
            after_snapshot.total_allocations - after_first_insert.total_allocations,
            1
        );
        assert_eq!(
            after_snapshot.array_allocations - after_first_insert.array_allocations,
            1
        );
        assert_eq!(after_snapshot.string_allocations, 0);
        eprintln!(
            "dictionary metric deltas: initial={initial:?}; header={after_header:?}; first_insert={after_first_insert:?}; snapshot={after_snapshot:?}"
        );
        assert!(context.take_error().is_none());
    }

    #[test]
    fn ten_thousand_empty_dictionaries_follow_arena_recovery_rules() {
        let mut temporary = ExecutionContext::with_stats();
        temporary.enter_temporary_scope();
        for _ in 0..10_000 {
            let dictionary = temporary.allocate_dictionary_in_region(
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                ListRegion::Temporary,
            );
            assert!(!dictionary.is_null());
        }
        let temporary_peak = temporary.memory_stats().clone();
        temporary.leave_temporary_scope();
        let temporary_after_rewind = temporary.memory_stats().clone();
        assert_eq!(temporary_after_rewind.used_bytes, 0);
        assert_eq!(temporary_after_rewind.object_allocations, 10_000);
        assert_eq!(temporary_after_rewind.string_allocations, 0);

        let mut persistent = ExecutionContext::with_stats();
        for _ in 0..10_000 {
            let dictionary = persistent.allocate_dictionary_in_region(
                DictionaryKeyKind::Int,
                4,
                4,
                1,
                4,
                4,
                2,
                ListRegion::Persistent,
            );
            assert!(!dictionary.is_null());
        }
        assert!(persistent.memory_stats().used_bytes > 0);
        assert_eq!(persistent.memory_stats().object_allocations, 10_000);
        assert_eq!(persistent.memory_stats().string_allocations, 0);
        eprintln!(
            "empty dictionary stress: temporary_peak={temporary_peak:?}; temporary_after_rewind={temporary_after_rewind:?}; persistent={:?}",
            persistent.memory_stats()
        );
    }

    #[test]
    fn list_rejects_non_power_of_two_alignment() {
        let mut context = ExecutionContext::new();
        let header = context.allocate_list_in_region(4, 3, 1, ListRegion::Persistent);
        assert!(header.is_null());
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("alignment"))
        );

        let mut context = ExecutionContext::new();
        let header = context.allocate_list_in_region(4, 0, 1, ListRegion::Persistent);
        assert!(header.is_null());
        assert!(context.take_error().is_some());
    }

    #[test]
    fn list_rejects_alignment_beyond_the_arena_maximum() {
        let mut context = ExecutionContext::new();
        // 32 is a valid power of two but exceeds `arena::MAX_ALIGN` (16);
        // without this check the value survives construction and only
        // panics later, inside `PagedArena::alloc`, the first time the
        // list's buffer is grown.
        let header = context.allocate_list_in_region(4, 32, 1, ListRegion::Persistent);
        assert!(header.is_null());
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("exceeds the arena's maximum"))
        );
    }

    #[test]
    fn temporary_list_requires_an_active_scope() {
        let mut context = ExecutionContext::new();
        let header = context.allocate_list_in_region(4, 4, 1, ListRegion::Temporary);
        assert!(header.is_null());
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("temporary scope"))
        );
    }

    #[test]
    fn temporary_list_succeeds_inside_an_active_scope_and_records_its_region() {
        let mut context = ExecutionContext::new();
        context.enter_temporary_scope();
        let header = context.allocate_list_in_region(8, 8, 42, ListRegion::Temporary);
        assert!(!header.is_null());
        // SAFETY: `header` was just allocated above and is not aliased.
        #[allow(unsafe_code)]
        let list = unsafe { &*header };
        assert_eq!(list.region(), ListRegion::Temporary);
        context.leave_temporary_scope();
    }

    #[test]
    fn independent_list_headers_do_not_share_storage() {
        let mut context = ExecutionContext::new();
        let first = context.allocate_list_in_region(4, 4, 1, ListRegion::Persistent);
        let second = context.allocate_list_in_region(8, 8, 2, ListRegion::Persistent);
        assert_ne!(first as *const (), second as *const ());
        // SAFETY: both headers were just allocated above and are not aliased.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*first).element_type_key(), 1);
            assert_eq!((*second).element_type_key(), 2);
        }
    }

    #[test]
    fn aliasing_a_list_reference_preserves_pointer_identity() {
        // At the MIR level `List<int> b = a;` lowers to a plain scalar copy
        // (`List<T>` is never aggregate, see `values::is_aggregate`), exactly
        // like `Class`/`Array`: an "alias" is, structurally, the same
        // pointer. Combined with `independent_list_headers_do_not_share_storage`
        // above (two distinct `new List<T>()` calls never share a header),
        // this is the full identity guarantee required by List B1.
        let mut context = ExecutionContext::new();
        let a = context.allocate_list_in_region(4, 4, 1, ListRegion::Persistent);
        let b = a;
        assert!(!a.is_null());
        assert_eq!(a, b);
        // SAFETY: `a` was just allocated above and is not aliased outside
        // this test; mutating through it and reading through `b` proves both
        // names observe the exact same storage.
        #[allow(unsafe_code)]
        unsafe {
            (*a).length = 7;
            assert_eq!((*b).length, 7);
        }
    }

    #[test]
    fn list_new_and_length_have_controlled_errors_for_null_pointers() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        assert!(aster_rt_list_new(std::ptr::null_mut(), 4, 4, 1).is_null());
        assert!(aster_rt_list_new_temporary(std::ptr::null_mut(), 4, 4, 1).is_null());
        assert_eq!(aster_rt_list_length(context_pointer, std::ptr::null()), 0);
        assert_eq!(
            aster_rt_list_length(std::ptr::null_mut(), std::ptr::null()),
            0
        );
    }

    #[test]
    fn list_new_produces_a_header_readable_through_list_length() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 42);
        assert!(!list.is_null());
        assert_eq!(aster_rt_list_length(context_pointer, list), 0);
        // SAFETY: `list` was just allocated above and is not aliased.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*list).region(), ListRegion::Persistent);
        }
    }

    #[test]
    fn list_new_temporary_requires_an_active_scope() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        assert!(aster_rt_list_new_temporary(context_pointer, 4, 4, 1).is_null());
        context.enter_temporary_scope();
        let list = aster_rt_list_new_temporary(context_pointer, 4, 4, 1);
        assert!(!list.is_null());
        // SAFETY: `list` was just allocated above and is not aliased.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*list).region(), ListRegion::Temporary);
        }
    }

    fn source_address(value: &i32) -> *const u8 {
        (std::ptr::from_ref(value)).cast::<u8>()
    }

    #[test]
    fn list_add_appends_and_increments_length() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 10_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        assert!(context.take_error().is_none());
        assert_eq!(aster_rt_list_length(context_pointer, list), 1);
        // SAFETY: `list` was just written to above and is not aliased.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*list).capacity(), 4);
            assert_eq!(std::ptr::read((*list).data.cast::<i32>()), 10);
        }
    }

    #[test]
    fn list_add_grows_geometrically_and_preserves_every_previous_element() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let expected_capacities = [4, 4, 4, 4, 8, 8, 8, 8, 16, 16, 16, 16, 16, 16, 16, 16, 32];
        for (index, &expected_capacity) in expected_capacities.iter().enumerate() {
            let value = i32::try_from(index).expect("small index");
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
            assert!(context.take_error().is_none(), "add {index} failed");
            // SAFETY: `list` was just written to above and is not aliased.
            #[allow(unsafe_code)]
            unsafe {
                assert_eq!((*list).capacity(), expected_capacity, "after add {index}");
            }
        }
        assert_eq!(
            aster_rt_list_length(context_pointer, list),
            i32::try_from(expected_capacities.len()).unwrap()
        );
        for index in 0..expected_capacities.len() {
            // SAFETY: every slot up to `length` was written by a successful
            // `aster_rt_list_add` above.
            #[allow(unsafe_code)]
            let stored = unsafe { std::ptr::read((*list).data.cast::<i32>().add(index)) };
            assert_eq!(stored, i32::try_from(index).unwrap(), "element {index}");
        }
    }

    #[test]
    fn list_add_does_not_grow_when_space_remains() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        // SAFETY: `list` was just written to above and is not aliased.
        #[allow(unsafe_code)]
        let data_after_first_add = unsafe { (*list).data };
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        // SAFETY: same as above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*list).data, data_after_first_add, "buffer must not move");
            assert_eq!((*list).capacity(), 4, "capacity must not change");
        }
    }

    #[test]
    fn list_add_rejects_a_null_list() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let value = 1_i32;
        aster_rt_list_add(
            context_pointer,
            std::ptr::null_mut(),
            4,
            4,
            1,
            source_address(&value),
        );
        assert!(context.take_error().is_some());
    }

    #[test]
    fn list_add_rejects_a_null_source() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        aster_rt_list_add(context_pointer, list, 4, 4, 1, std::ptr::null());
        assert!(context.take_error().is_some());
        assert_eq!(aster_rt_list_length(context_pointer, list), 0);
    }

    #[test]
    fn list_add_rejects_element_size_mismatch() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 8, 4, 1, source_address(&value));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("size mismatch"))
        );
        assert_eq!(aster_rt_list_length(context_pointer, list), 0);
    }

    #[test]
    fn list_add_rejects_element_align_mismatch() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 8, 1, source_address(&value));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("alignment mismatch"))
        );
        assert_eq!(aster_rt_list_length(context_pointer, list), 0);
    }

    #[test]
    fn list_add_rejects_element_type_key_mismatch() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 2, source_address(&value));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("type key mismatch"))
        );
        assert_eq!(aster_rt_list_length(context_pointer, list), 0);
    }

    #[test]
    fn list_add_rejects_a_header_with_negative_length() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        // SAFETY: `list` was just allocated above; this simulates a
        // corrupted header for a synthetic MIR/runtime test.
        #[allow(unsafe_code)]
        unsafe {
            (*list).length = -1;
        }
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("negative length"))
        );
    }

    #[test]
    fn list_add_rejects_a_header_with_length_greater_than_capacity() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        // SAFETY: same as above.
        #[allow(unsafe_code)]
        unsafe {
            (*list).length = 5;
        }
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("length greater than"))
        );
    }

    #[test]
    fn list_add_rejects_a_header_with_positive_capacity_but_null_data() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        // SAFETY: same as above.
        #[allow(unsafe_code)]
        unsafe {
            (*list).capacity = 4;
        }
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("null data pointer"))
        );
    }

    #[test]
    fn list_add_rejects_a_header_with_alignment_beyond_the_arena_maximum() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        // SAFETY: simulates a corrupted header reaching `list.Add` with an
        // element alignment the arena cannot satisfy (`PagedArena::alloc`
        // asserts `align <= MAX_ALIGN`); the growth path (length == capacity
        // == 0 here) is exactly where this would otherwise trigger a real,
        // uncontrolled panic instead of a reported runtime error.
        #[allow(unsafe_code)]
        unsafe {
            (*list).element_align = 32;
        }
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 32, 1, source_address(&value));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("exceeds the arena's maximum"))
        );
        assert_eq!(aster_rt_list_length(context_pointer, list), 0);
    }

    #[test]
    fn list_add_rejects_capacity_overflow_while_growing() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        // SAFETY: simulates a list that has already grown to the edge of
        // `i32`, so the next doubling must overflow â€” this is otherwise
        // unreachable without billions of real `Add` calls.
        #[allow(unsafe_code)]
        unsafe {
            (*list).length = i32::MAX;
            (*list).capacity = i32::MAX;
            (*list).data = source_address(&1_i32).cast_mut();
        }
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("capacity overflow"))
        );
        assert_eq!(aster_rt_list_length(context_pointer, list), i32::MAX);
    }

    #[test]
    fn list_add_requires_an_active_temporary_scope_to_grow_a_temporary_list() {
        // A temporary list cannot realistically outlive its creating scope
        // (leaving the scope zeroes its header's memory, so a stale header
        // is caught by the "zero element size" check well before this one).
        // This instead simulates the only other way the runtime could see a
        // well-formed `Temporary`-region header with no active scope: a
        // persistent (never-rewound) header whose `region` field a synthetic
        // MIR/runtime test set to `Temporary` directly.
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        // SAFETY: `list` was just allocated above and is not aliased.
        #[allow(unsafe_code)]
        unsafe {
            (*list).region = ListRegion::Temporary;
        }
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("temporary list growth"))
        );
        assert_eq!(aster_rt_list_length(context_pointer, list), 0);
    }

    #[test]
    fn list_add_stores_reference_identity_not_a_structural_copy() {
        // A "class reference" element is, at this ABI layer, just a pointer;
        // `Add` must copy that pointer's bits (identity), never dereference
        // or clone whatever it points to.
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let pointer_size = i32::try_from(std::mem::size_of::<*const i32>()).unwrap();
        let pointer_align = i32::try_from(std::mem::align_of::<*const i32>()).unwrap();
        let list = aster_rt_list_new(context_pointer, pointer_size, pointer_align, 7);
        let referenced = 42_i32;
        let reference: *const i32 = &raw const referenced;
        let reference_address = std::ptr::from_ref(&reference).cast::<u8>();
        aster_rt_list_add(
            context_pointer,
            list,
            pointer_size,
            pointer_align,
            7,
            reference_address,
        );
        assert!(context.take_error().is_none());
        // SAFETY: `list` was just written to above and is not aliased.
        #[allow(unsafe_code)]
        unsafe {
            let stored = std::ptr::read((*list).data.cast::<*const i32>());
            assert_eq!(
                stored, reference,
                "stored pointer must be the same identity"
            );
            assert_eq!(*stored, 42);
        }
    }

    fn destination(value: &mut i32) -> *mut u8 {
        (std::ptr::from_mut(value)).cast::<u8>()
    }

    #[test]
    fn list_get_reads_the_correct_element() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        for value in [10_i32, 20, 30] {
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        assert!(context.take_error().is_none());
        for (index, expected) in [(0_i32, 10_i32), (1, 20), (2, 30)] {
            let mut out = 0_i32;
            aster_rt_list_get(context_pointer, list, 4, 4, 1, index, destination(&mut out));
            assert!(context.take_error().is_none(), "Get({index}) failed");
            assert_eq!(out, expected, "Get({index})");
        }
    }

    #[test]
    fn list_get_does_not_modify_the_list() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 4, 1, 0, destination(&mut out));
        assert_eq!(aster_rt_list_length(context_pointer, list), 1);
        // SAFETY: `list` is valid and not aliased.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*list).capacity(), 4);
        }
    }

    #[test]
    fn list_get_rejects_a_null_list() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let mut out = 0_i32;
        aster_rt_list_get(
            context_pointer,
            std::ptr::null(),
            4,
            4,
            1,
            0,
            destination(&mut out),
        );
        assert!(context.take_error().is_some());
    }

    #[test]
    fn list_get_rejects_a_null_destination() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        aster_rt_list_get(context_pointer, list, 4, 4, 1, 0, std::ptr::null_mut());
        assert!(context.take_error().is_some());
    }

    #[test]
    fn list_get_rejects_element_size_mismatch() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 8, 4, 1, 0, destination(&mut out));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("size mismatch"))
        );
    }

    #[test]
    fn list_get_rejects_element_align_mismatch() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 8, 1, 0, destination(&mut out));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("alignment mismatch"))
        );
    }

    #[test]
    fn list_get_rejects_element_type_key_mismatch() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 4, 2, 0, destination(&mut out));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("type key mismatch"))
        );
    }

    #[test]
    fn list_get_rejects_a_negative_index() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 4, 1, -1, destination(&mut out));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("negative"))
        );
    }

    #[test]
    fn list_get_rejects_an_index_on_an_empty_list() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 4, 1, 0, destination(&mut out));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("out of bounds"))
        );
    }

    #[test]
    fn list_get_rejects_an_index_equal_to_length() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 4, 1, 1, destination(&mut out));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("out of bounds"))
        );
    }

    #[test]
    fn list_get_rejects_an_index_far_beyond_length() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 4, 1, 1000, destination(&mut out));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("out of bounds"))
        );
    }

    #[test]
    fn list_get_rejects_a_header_with_negative_length() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        // SAFETY: `list` was just allocated above; this simulates a
        // corrupted header for a synthetic MIR/runtime test.
        #[allow(unsafe_code)]
        unsafe {
            (*list).length = -1;
        }
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 4, 1, 0, destination(&mut out));
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("negative length"))
        );
    }

    #[test]
    fn list_get_error_does_not_contaminate_a_later_valid_call() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 7_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 4, 1, 5, destination(&mut out));
        assert!(context.take_error().is_some());
        aster_rt_list_get(context_pointer, list, 4, 4, 1, 0, destination(&mut out));
        assert!(context.take_error().is_none());
        assert_eq!(out, 7);
    }

    #[test]
    fn list_get_does_not_grow_the_arena_across_many_calls() {
        let mut context = ExecutionContext::with_stats();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        let used_before = context.memory_stats().used_bytes;
        for _ in 0..1000 {
            let mut out = 0_i32;
            aster_rt_list_get(context_pointer, list, 4, 4, 1, 0, destination(&mut out));
        }
        assert!(context.take_error().is_none());
        assert_eq!(context.memory_stats().used_bytes, used_before);
    }

    #[test]
    fn list_remove_at_shifts_later_elements_left() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        for value in [10_i32, 20, 30, 40] {
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 1);
        assert!(context.take_error().is_none());
        assert_eq!(aster_rt_list_length(context_pointer, list), 3);
        for (index, expected) in [(0_i32, 10_i32), (1, 30), (2, 40)] {
            let mut out = 0_i32;
            aster_rt_list_get(context_pointer, list, 4, 4, 1, index, destination(&mut out));
            assert_eq!(out, expected, "Get({index}) after RemoveAt(1)");
        }
    }

    #[test]
    fn list_remove_at_clears_the_old_last_slot() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        for value in [10_i32, 20, 30] {
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 0);
        assert!(context.take_error().is_none());
        // SAFETY: `list` is valid and not aliased; offset 2 (the pre-removal
        // last index) is within the buffer's allocated capacity.
        #[allow(unsafe_code)]
        unsafe {
            let old_last = (*list).data.cast::<i32>().add(2);
            assert_eq!(std::ptr::read(old_last), 0, "old last slot must be zeroed");
        }
    }

    #[test]
    fn list_remove_at_does_not_change_capacity_or_data() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        for value in [1_i32, 2, 3] {
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        // SAFETY: `list` is valid and not aliased.
        #[allow(unsafe_code)]
        let (capacity_before, data_before) = unsafe { ((*list).capacity(), (*list).data) };
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 1);
        assert!(context.take_error().is_none());
        // SAFETY: same as above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*list).capacity(), capacity_before);
            assert_eq!((*list).data, data_before);
        }
    }

    #[test]
    fn list_remove_at_does_not_allocate() {
        let mut context = ExecutionContext::with_stats();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        for value in [1_i32, 2, 3, 4] {
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        let used_before = context.memory_stats().used_bytes;
        for _ in 0..1000 {
            aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 0);
            let value = 1_i32;
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        assert!(context.take_error().is_none());
        assert_eq!(context.memory_stats().used_bytes, used_before);
    }

    #[test]
    fn list_remove_at_and_add_reuse_the_same_buffer() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        for value in [1_i32, 2, 3, 4] {
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        // SAFETY: `list` is valid and not aliased.
        #[allow(unsafe_code)]
        let data_before = unsafe { (*list).data };
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 0);
        let value = 99_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        assert!(context.take_error().is_none());
        // SAFETY: same as above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(
                (*list).data,
                data_before,
                "buffer must be reused, not reallocated"
            );
            assert_eq!((*list).capacity(), 4);
        }
    }

    #[test]
    fn list_remove_at_removing_every_element_leaves_a_valid_empty_list() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        for value in [1_i32, 2, 3] {
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        for _ in 0..3 {
            aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 0);
        }
        assert!(context.take_error().is_none());
        assert_eq!(aster_rt_list_length(context_pointer, list), 0);
        // SAFETY: `list` is valid and not aliased.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*list).capacity(), 4, "capacity is preserved, never shrunk");
        }
        let value = 42_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        assert!(context.take_error().is_none());
        assert_eq!(aster_rt_list_length(context_pointer, list), 1);
    }

    #[test]
    fn list_remove_at_rejects_a_null_list() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        aster_rt_list_remove_at(context_pointer, std::ptr::null_mut(), 4, 4, 1, 0);
        assert!(context.take_error().is_some());
    }

    #[test]
    fn list_remove_at_rejects_element_size_mismatch() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        aster_rt_list_remove_at(context_pointer, list, 8, 4, 1, 0);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("size mismatch"))
        );
        assert_eq!(aster_rt_list_length(context_pointer, list), 1);
    }

    #[test]
    fn list_remove_at_rejects_element_align_mismatch() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        aster_rt_list_remove_at(context_pointer, list, 4, 8, 1, 0);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("alignment mismatch"))
        );
    }

    #[test]
    fn list_remove_at_rejects_element_type_key_mismatch() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 2, 0);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("type key mismatch"))
        );
    }

    #[test]
    fn list_remove_at_rejects_a_negative_index() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, -1);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("negative"))
        );
        assert_eq!(aster_rt_list_length(context_pointer, list), 1);
    }

    #[test]
    fn list_remove_at_rejects_an_index_on_an_empty_list() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 0);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("out of bounds"))
        );
    }

    #[test]
    fn list_remove_at_rejects_an_index_equal_to_length() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 1);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("out of bounds"))
        );
        assert_eq!(aster_rt_list_length(context_pointer, list), 1);
    }

    #[test]
    fn list_remove_at_rejects_an_index_far_beyond_length() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let value = 1_i32;
        aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 1000);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("out of bounds"))
        );
    }

    #[test]
    fn list_remove_at_rejects_a_header_with_negative_length() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        // SAFETY: `list` was just allocated above; this simulates a
        // corrupted header for a synthetic MIR/runtime test.
        #[allow(unsafe_code)]
        unsafe {
            (*list).length = -1;
        }
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 0);
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("negative length"))
        );
    }

    #[test]
    fn list_remove_at_error_leaves_the_buffer_byte_for_byte_unchanged() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        for value in [10_i32, 20, 30] {
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        // SAFETY: `list` is valid and not aliased.
        #[allow(unsafe_code)]
        let snapshot = unsafe {
            [
                std::ptr::read((*list).data.cast::<i32>()),
                std::ptr::read((*list).data.cast::<i32>().add(1)),
                std::ptr::read((*list).data.cast::<i32>().add(2)),
            ]
        };
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 50);
        assert!(context.take_error().is_some());
        assert_eq!(aster_rt_list_length(context_pointer, list), 3);
        // SAFETY: same as above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(
                [
                    std::ptr::read((*list).data.cast::<i32>()),
                    std::ptr::read((*list).data.cast::<i32>().add(1)),
                    std::ptr::read((*list).data.cast::<i32>().add(2)),
                ],
                snapshot
            );
        }
    }

    #[test]
    fn list_remove_at_error_does_not_contaminate_a_later_valid_call() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        for value in [10_i32, 20] {
            aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
        }
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 5);
        assert!(context.take_error().is_some());
        aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 0);
        assert!(context.take_error().is_none());
        assert_eq!(aster_rt_list_length(context_pointer, list), 1);
        let mut out = 0_i32;
        aster_rt_list_get(context_pointer, list, 4, 4, 1, 0, destination(&mut out));
        assert_eq!(out, 20);
    }

    /// A tiny fixed, non-cryptographic linear congruential generator so the
    /// operation sequence below is exactly reproducible on failure (no `rand`
    /// dependency, no time-based input, no external state).
    fn next_deterministic(seed: &mut u32) -> u32 {
        *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        *seed >> 16
    }

    #[test]
    fn list_matches_a_reference_vec_model_over_a_deterministic_operation_sequence() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let list = aster_rt_list_new(context_pointer, 4, 4, 1);
        let mut model: Vec<i32> = Vec::new();
        let mut seed = 0xC0FF_EE42_u32;

        let assert_matches_model =
            |context_pointer: *mut ExecutionContext, list: *mut AsterList, model: &[i32]| {
                assert_eq!(
                    aster_rt_list_length(context_pointer, list),
                    i32::try_from(model.len()).unwrap()
                );
                for (index, &expected) in model.iter().enumerate() {
                    let mut out = 0_i32;
                    aster_rt_list_get(
                        context_pointer,
                        list,
                        4,
                        4,
                        1,
                        i32::try_from(index).unwrap(),
                        destination(&mut out),
                    );
                    assert_eq!(out, expected, "mismatch at index {index}: {model:?}");
                }
            };

        for step in 0_i32..300 {
            match next_deterministic(&mut seed) % 5 {
                0 | 1 => {
                    // Add: value is a pure function of `step`, so the
                    // expected content is reconstructible from the step
                    // count alone.
                    let value = step * 7 + 3;
                    aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
                    assert!(context.take_error().is_none());
                    model.push(value);
                }
                2 if !model.is_empty() => {
                    let index = (next_deterministic(&mut seed) as usize) % model.len();
                    let mut out = 0_i32;
                    aster_rt_list_get(
                        context_pointer,
                        list,
                        4,
                        4,
                        1,
                        i32::try_from(index).unwrap(),
                        destination(&mut out),
                    );
                    assert!(context.take_error().is_none());
                    assert_eq!(out, model[index]);
                }
                3 if !model.is_empty() => {
                    let index = (next_deterministic(&mut seed) as usize) % model.len();
                    aster_rt_list_remove_at(
                        context_pointer,
                        list,
                        4,
                        4,
                        1,
                        i32::try_from(index).unwrap(),
                    );
                    assert!(context.take_error().is_none());
                    model.remove(index);
                }
                _ => {
                    // Either the `2`/`3` guard failed on an empty list, or we
                    // rolled `4`: exercise an out-of-bounds `Get`/`RemoveAt`
                    // and confirm the error is controlled and the model-
                    // observable state is untouched.
                    let length_before = aster_rt_list_length(context_pointer, list);
                    let bad_index = length_before + 1;
                    let mut out = -1_i32;
                    aster_rt_list_get(
                        context_pointer,
                        list,
                        4,
                        4,
                        1,
                        bad_index,
                        destination(&mut out),
                    );
                    assert!(context.take_error().is_some());
                    aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, bad_index);
                    assert!(context.take_error().is_some());
                    assert_eq!(aster_rt_list_length(context_pointer, list), length_before);
                }
            }
            assert_matches_model(context_pointer, list, &model);
        }

        assert!(
            model.len() > 30,
            "the deterministic sequence should have exercised substantial growth, got {}",
            model.len()
        );
    }

    #[test]
    fn a_list_error_in_one_context_does_not_contaminate_an_independent_context() {
        let mut first = ExecutionContext::new();
        let first_pointer = &raw mut first;
        let first_list = aster_rt_list_new(first_pointer, 4, 4, 1);
        aster_rt_list_remove_at(first_pointer, first_list, 4, 4, 1, 0);
        assert!(first.take_error().is_some());

        let mut second = ExecutionContext::new();
        let second_pointer = &raw mut second;
        let second_list = aster_rt_list_new(second_pointer, 4, 4, 1);
        let value = 99_i32;
        aster_rt_list_add(second_pointer, second_list, 4, 4, 1, source_address(&value));
        assert!(second.take_error().is_none());
        assert_eq!(aster_rt_list_length(second_pointer, second_list), 1);
    }

    #[test]
    fn a_temporary_list_scope_reclaims_every_generation_of_its_buffer_on_reset() {
        // Many create/add/read/remove/empty cycles inside repeated temporary
        // scopes: each cycle grows a temporary list through several buffer
        // generations, so leaving the scope must reclaim the header, every
        // superseded growth buffer, and the final buffer alike (Section 6).
        let mut context = ExecutionContext::with_stats();
        let context_pointer = &raw mut context;
        let baseline_used = context.memory_stats().used_bytes;

        for _cycle in 0..5 {
            context.enter_temporary_scope();
            let list = aster_rt_list_new_temporary(context_pointer, 4, 4, 1);
            assert!(!list.is_null());
            for value in 0_i32..40 {
                aster_rt_list_add(context_pointer, list, 4, 4, 1, source_address(&value));
            }
            assert!(context.take_error().is_none());
            for index in 0_i32..40 {
                let mut out = 0_i32;
                aster_rt_list_get(context_pointer, list, 4, 4, 1, index, destination(&mut out));
            }
            for _ in 0_i32..40 {
                aster_rt_list_remove_at(context_pointer, list, 4, 4, 1, 0);
            }
            assert_eq!(aster_rt_list_length(context_pointer, list), 0);
            assert!(
                context.memory_stats().used_bytes > baseline_used,
                "the scope should be using more memory than baseline while active"
            );
            context.leave_temporary_scope();
            assert!(context.take_error().is_none());
            assert_eq!(
                context.memory_stats().used_bytes,
                baseline_used,
                "leaving the scope must reclaim the header and every growth generation, \
                 not just the final buffer"
            );
        }
    }

    #[test]
    fn allocation_is_zeroed_and_bounds_errors_are_controlled() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let array = aster_rt_array_new(context_pointer, 2, 4);
        assert_eq!(aster_rt_array_length(context_pointer, array), 2);
        let element = aster_rt_array_element(context_pointer, array, 0);
        assert!(!element.is_null());
        // SAFETY: element points to zeroed arena memory with space for a u32.
        #[allow(unsafe_code)]
        let value = unsafe { std::ptr::read(element as *const u32) };
        assert_eq!(value, 0);
        assert!(!aster_rt_array_element(context_pointer, array, 2).is_null());
        assert!(context.take_error().unwrap().contains("outside"));
    }

    #[test]
    fn array_header_offsets_follow_the_repr_c_layout() {
        assert_eq!(
            ASTER_ARRAY_DATA_OFFSET,
            std::mem::offset_of!(AsterArray, data)
        );
        assert_eq!(
            ASTER_ARRAY_LENGTH_OFFSET,
            std::mem::offset_of!(AsterArray, length)
        );
    }

    #[test]
    fn object_storage_is_zeroed_and_owned_by_the_context() {
        let mut context = ExecutionContext::new();
        let pointer = aster_rt_object_new(&raw mut context, 16);
        assert!(!pointer.is_null());
        // SAFETY: pointer points to 16 bytes of zeroed memory owned by the arena.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(pointer, 16) };
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn stats_disabled_by_default() {
        let mut context = ExecutionContext::new();
        aster_rt_object_new(&raw mut context, 16);
        aster_rt_array_new(&raw mut context, 4, 4);
        context.allocate_string_parts(&["hello"]);
        assert_eq!(*context.memory_stats(), MemoryStats::default());
    }

    #[test]
    fn object_allocation_increments_stats() {
        let mut context = ExecutionContext::with_stats();
        aster_rt_object_new(&raw mut context, 16);
        let stats = context.memory_stats();
        assert_eq!(stats.total_allocations, 1);
        assert_eq!(stats.object_allocations, 1);
        assert_eq!(stats.array_allocations, 0);
        assert_eq!(stats.string_allocations, 0);
        assert_eq!(stats.requested_bytes, 16);
        assert!(stats.used_bytes >= 16);
        assert!(stats.reserved_bytes >= stats.used_bytes);
        assert_eq!(stats.peak_used_bytes, stats.used_bytes);
    }

    #[test]
    fn array_is_one_logical_allocation() {
        let mut context = ExecutionContext::with_stats();
        aster_rt_array_new(&raw mut context, 4, 4);
        let stats = context.memory_stats();
        assert_eq!(stats.total_allocations, 1);
        assert_eq!(stats.array_allocations, 1);
        assert_eq!(stats.object_allocations, 0);
        assert!(stats.requested_bytes >= 16);
        assert!(stats.used_bytes >= stats.requested_bytes);
        assert!(stats.reserved_bytes >= stats.used_bytes);
    }

    #[test]
    fn array_allocation_preflights_the_complete_header_and_payload() {
        let data_offset = checked_align_up(size_of::<AsterArray>(), 8).expect("array data offset");
        let maximum_payload = MIN_PAGE_SIZE - data_offset;
        let maximum_length = i32::try_from(maximum_payload / 4).expect("test length");

        let mut valid = ExecutionContext::with_options(true, MIN_PAGE_SIZE);
        let array = valid.allocate_array(maximum_length, 4);
        assert!(!array.is_null());
        assert!(valid.take_error().is_none());
        assert_eq!(valid.memory_stats().array_allocations, 1);
        assert_eq!(valid.memory_stats().requested_bytes, maximum_payload as u64);
        assert_eq!(valid.memory_stats().used_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(valid.memory_stats().reserved_bytes, MIN_PAGE_SIZE as u64);

        for length in [maximum_length + 4, maximum_length + 5] {
            let mut rejected = ExecutionContext::with_options(true, MIN_PAGE_SIZE);
            assert!(rejected.allocate_array(length, 4).is_null());
            assert_eq!(rejected.memory_stats(), &MemoryStats::default());
            assert_eq!(
                rejected.take_error().as_deref(),
                Some("allocation exceeds the execution memory limit of 4096 bytes")
            );
        }
    }

    #[test]
    fn zero_length_and_ordinary_arrays_keep_their_allocation_contract() {
        let mut context = ExecutionContext::with_stats();
        let empty = context.allocate_array(0, 4);
        let ordinary = context.allocate_array(3, 4);
        assert!(!empty.is_null());
        assert!(!ordinary.is_null());
        assert_eq!(context.memory_stats().array_allocations, 2);
        assert_eq!(context.memory_stats().requested_bytes, 16);
        assert!(context.take_error().is_none());
    }

    #[test]
    fn array_layout_overflow_and_preflight_errors_are_fail_closed_and_first_wins() {
        assert!(array_allocation_layout(usize::MAX, 2).is_none());
        assert!(array_allocation_layout(usize::MAX, 1).is_none());

        let mut first = ExecutionContext::with_options(true, MIN_PAGE_SIZE);
        first.fail("earlier failure");
        assert!(
            first
                .allocate_array(i32::try_from(MIN_PAGE_SIZE).unwrap(), 1)
                .is_null()
        );
        assert_eq!(first.take_error().as_deref(), Some("earlier failure"));
        assert_eq!(first.memory_stats(), &MemoryStats::default());
    }

    #[test]
    fn string_allocation_increments_stats() {
        let mut context = ExecutionContext::with_stats();
        context.allocate_string_parts(&["hello"]);
        let stats = context.memory_stats();
        assert_eq!(stats.total_allocations, 1);
        assert_eq!(stats.string_allocations, 1);
        assert_eq!(stats.object_allocations, 0);
        assert!(stats.requested_bytes > 0);
    }

    #[test]
    fn string_builder_grows_geometrically_and_snapshots_are_immutable() {
        let mut context = ExecutionContext::with_stats();
        let a = context.allocate_string_parts(&["a"]);
        let b = context.allocate_string_parts(&["b"]);
        let builder = context.allocate_string_builder_in_region(ListRegion::Persistent);

        context.string_builder_append(builder, a);
        let first = context.string_builder_to_string(builder, false);
        context.string_builder_append(builder, b);
        let second = context.string_builder_to_string(builder, false);

        // SAFETY: all pointers remain owned by the live persistent arena.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(crate::string::view(first), Some("a"));
            assert_eq!(crate::string::view(second), Some("ab"));
        }
        assert_eq!(context.memory_stats().string_allocations, 4);
        assert_eq!(context.memory_stats().object_allocations, 2);
        assert!(context.take_error().is_none());
    }

    #[test]
    fn string_builder_preserves_utf8_and_empty_appends() {
        let mut context = ExecutionContext::new();
        let builder = context.allocate_string_builder_in_region(ListRegion::Persistent);
        for part in ["", "A", "á", "😀", "e\u{301}"] {
            let value = context.allocate_string_parts(&[part]);
            context.string_builder_append(builder, value);
        }
        let result = context.string_builder_to_string(builder, false);
        // SAFETY: the result is a live immutable arena string.
        #[allow(unsafe_code)]
        let result = unsafe { crate::string::view(result) };
        assert_eq!(result, Some("Aá😀e\u{301}"));
        assert!(context.take_error().is_none());
    }

    #[test]
    fn string_builder_growth_failure_is_atomic_and_first_error_wins() {
        assert!(string_builder_growth_capacity(usize::MAX / 2 + 2).is_none());

        let mut source = ExecutionContext::new();
        let large = source.allocate_string_parts(&[&"x".repeat(MIN_PAGE_SIZE + 1)]);
        let mut limited = ExecutionContext::with_allocation_limit(MIN_PAGE_SIZE);
        let builder = limited.allocate_string_builder_in_region(ListRegion::Persistent);
        assert!(!builder.is_null());

        limited.string_builder_append(builder, large);
        assert!(
            limited
                .take_error()
                .is_some_and(|error| error.contains("execution memory limit"))
        );
        let empty = limited.string_builder_to_string(builder, false);
        // SAFETY: failed growth did not publish pointer/length/capacity.
        #[allow(unsafe_code)]
        let empty = unsafe { crate::string::view(empty) };
        assert_eq!(empty, Some(""));

        limited.fail("earlier builder failure");
        limited.string_builder_append(builder, large);
        assert_eq!(
            limited.take_error().as_deref(),
            Some("earlier builder failure")
        );
    }

    #[test]
    fn string_builder_length_overflow_fails_before_touching_storage() {
        let mut context = ExecutionContext::new();
        let value = context.allocate_string_parts(&["x"]);
        let builder = context.allocate_string_builder_in_region(ListRegion::Persistent);
        // This impossible internal state models the arithmetic boundary
        // directly without allocating anywhere near the context limit. The
        // append must reject `length + input.len()` before dereferencing data.
        #[allow(unsafe_code)]
        unsafe {
            (*builder).data = std::ptr::NonNull::<u8>::dangling().as_ptr();
            (*builder).length = usize::MAX;
            (*builder).capacity = usize::MAX;
        }
        context.string_builder_append(builder, value);
        assert_eq!(
            context.take_error().as_deref(),
            Some("StringBuilder length overflow while appending")
        );
    }

    #[test]
    fn string_builder_snapshot_failure_preserves_builder_content() {
        let mut source = ExecutionContext::new();
        let value = source.allocate_string_parts(&[&"x".repeat(2_010)]);
        let mut limited = ExecutionContext::with_allocation_limit(MIN_PAGE_SIZE);
        let builder = limited.allocate_string_builder_in_region(ListRegion::Persistent);
        assert!(!builder.is_null());
        limited.string_builder_append(builder, value);
        assert!(limited.take_error().is_none());

        assert!(limited.string_builder_to_string(builder, false).is_null());
        assert!(
            limited
                .take_error()
                .is_some_and(|error| error.contains("execution memory limit"))
        );
        // SAFETY: ToString never mutates the builder header, including when
        // its exact-size immutable allocation fails.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*builder).length, 2_010);
            assert_eq!(
                std::slice::from_raw_parts((*builder).data, (*builder).length),
                "x".repeat(2_010).as_bytes()
            );
        }
    }

    #[test]
    fn temporary_string_builder_rewinds_and_nested_growth_stays_live() {
        let mut context = ExecutionContext::with_stats();
        let value = context.allocate_string_parts(&["nested"]);
        aster_rt_temporary_scope_enter(&raw mut context);
        let builder = context.allocate_string_builder_in_region(ListRegion::Temporary);
        aster_rt_temporary_scope_enter(&raw mut context);
        context.string_builder_append(builder, value);
        aster_rt_temporary_scope_leave(&raw mut context);

        let snapshot = context.string_builder_to_string(builder, true);
        // SAFETY: nested growth was promoted and the outer temporary header
        // remains live until the outer scope leaves below.
        #[allow(unsafe_code)]
        let snapshot = unsafe { crate::string::view(snapshot) };
        assert_eq!(snapshot, Some("nested"));
        aster_rt_temporary_scope_leave(&raw mut context);
        assert!(context.memory_stats().used_bytes > 0);
        assert!(context.take_error().is_none());

        let mut local = ExecutionContext::with_stats();
        let value = local.allocate_string_parts(&["local"]);
        aster_rt_temporary_scope_enter(&raw mut local);
        let builder = local.allocate_string_builder_in_region(ListRegion::Temporary);
        local.string_builder_append(builder, value);
        assert!(!local.string_builder_to_string(builder, true).is_null());
        aster_rt_temporary_scope_leave(&raw mut local);
        assert_eq!(local.memory_stats().used_bytes, 13);
    }

    #[test]
    fn multiple_allocations_accumulate() {
        let mut context = ExecutionContext::with_stats();
        aster_rt_object_new(&raw mut context, 8);
        aster_rt_object_new(&raw mut context, 8);
        aster_rt_array_new(&raw mut context, 2, 4);
        let stats = context.memory_stats();
        assert_eq!(stats.total_allocations, 3);
        assert_eq!(stats.object_allocations, 2);
        assert_eq!(stats.array_allocations, 1);
    }

    #[test]
    fn peaks_are_never_below_final_values() {
        let mut context = ExecutionContext::with_stats();
        aster_rt_object_new(&raw mut context, 64);
        aster_rt_array_new(&raw mut context, 10, 8);
        context.allocate_string_parts(&["test"]);
        let stats = context.memory_stats();
        assert!(stats.peak_used_bytes >= stats.used_bytes);
        assert!(stats.peak_reserved_bytes >= stats.reserved_bytes);
    }

    #[test]
    fn fresh_context_has_zero_stats() {
        let context = ExecutionContext::new();
        assert_eq!(*context.memory_stats(), MemoryStats::default());
        let context_with = ExecutionContext::with_stats();
        assert_eq!(*context_with.memory_stats(), MemoryStats::default());
    }

    #[test]
    fn requested_used_reserved_ordering() {
        let mut context = ExecutionContext::with_stats();
        aster_rt_object_new(&raw mut context, 3);
        let stats = context.memory_stats();
        assert!(stats.requested_bytes <= stats.used_bytes);
        assert!(stats.used_bytes <= stats.reserved_bytes);
    }

    #[test]
    fn two_objects_have_separate_storage() {
        let mut context = ExecutionContext::new();
        let ctx = &raw mut context;
        let a = aster_rt_object_new(ctx, 32);
        let b = aster_rt_object_new(ctx, 32);
        assert_ne!(a, b);
        // SAFETY: both pointers are arena-owned, non-overlapping.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(a as *mut u64, 0xAAAA);
            std::ptr::write(b as *mut u64, 0xBBBB);
            assert_eq!(std::ptr::read(a as *const u64), 0xAAAA);
            assert_eq!(std::ptr::read(b as *const u64), 0xBBBB);
        }
    }

    #[test]
    fn objects_cross_multiple_pages() {
        let mut context = ExecutionContext::with_stats();
        let ctx = &raw mut context;
        let mut ptrs = Vec::new();
        for _ in 0..2048 {
            let p = aster_rt_object_new(ctx, 64);
            assert!(!p.is_null());
            ptrs.push(p);
        }
        // SAFETY: verify first and last are zeroed and distinct.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read(ptrs[0] as *const u8), 0);
            assert_eq!(std::ptr::read(*ptrs.last().unwrap() as *const u8), 0);
        }
        assert_ne!(ptrs[0], *ptrs.last().unwrap());
        let stats = context.memory_stats();
        assert_eq!(stats.object_allocations, 2048);
        assert!(stats.reserved_bytes > 64 * 1024);
    }

    #[test]
    fn string_valid_after_other_allocations() {
        let mut context = ExecutionContext::new();
        let s = context.allocate_string_parts(&["hello"]);
        assert!(!s.is_null());
        aster_rt_object_new(&raw mut context, 128);
        aster_rt_object_new(&raw mut context, 128);
        // SAFETY: string pointer is arena-owned and stable.
        #[allow(unsafe_code)]
        let text = unsafe { crate::string::view(s) };
        assert_eq!(text, Some("hello"));
    }

    #[test]
    fn two_strings_have_separate_storage() {
        let mut context = ExecutionContext::new();
        let a = context.allocate_string_parts(&["alpha"]);
        let b = context.allocate_string_parts(&["beta"]);
        assert_ne!(a, b);
        // SAFETY: both strings are arena-owned.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(crate::string::view(a), Some("alpha"));
            assert_eq!(crate::string::view(b), Some("beta"));
        }
    }

    #[test]
    fn strings_cross_multiple_pages() {
        let mut context = ExecutionContext::with_stats();
        let long_part = "x".repeat(1024);
        let mut ptrs = Vec::new();
        for _ in 0..128 {
            let p = context.allocate_string_parts(&[&long_part]);
            assert!(!p.is_null());
            ptrs.push(p);
        }
        // SAFETY: first string is still valid.
        #[allow(unsafe_code)]
        let text = unsafe { crate::string::view(ptrs[0]) };
        assert_eq!(text, Some(long_part.as_str()));
        assert_eq!(context.memory_stats().string_allocations, 128);
    }

    #[test]
    fn array_header_and_data_valid_after_new_pages() {
        let mut context = ExecutionContext::new();
        let ctx = &raw mut context;
        let array = aster_rt_array_new(ctx, 4, 4);
        for _ in 0..4 {
            aster_rt_object_new(ctx, 32768);
        }
        assert_eq!(aster_rt_array_length(ctx, array), 4);
        let elem = aster_rt_array_element(ctx, array, 3);
        assert!(!elem.is_null());
        // SAFETY: element still points to valid zeroed memory.
        #[allow(unsafe_code)]
        let value = unsafe { std::ptr::read(elem as *const u32) };
        assert_eq!(value, 0);
    }

    #[test]
    fn large_array_exceeds_default_page() {
        let mut context = ExecutionContext::with_stats();
        let ctx = &raw mut context;
        let array = aster_rt_array_new(ctx, 100_000, 8);
        assert!(!array.is_null());
        assert_eq!(aster_rt_array_length(ctx, array), 100_000);
        let last = aster_rt_array_element(ctx, array, 99_999);
        assert!(!last.is_null());
        assert!(context.take_error().is_none());
        let stats = context.memory_stats();
        assert_eq!(stats.array_allocations, 1);
        assert!(stats.reserved_bytes >= 800_000);
    }

    #[test]
    fn arena_metrics_known_sequence() {
        let mut context = ExecutionContext::with_stats();
        aster_rt_object_new(&raw mut context, 16);
        let stats = context.memory_stats();
        assert_eq!(stats.requested_bytes, 16);
        assert_eq!(stats.used_bytes, 16);
        assert_eq!(stats.reserved_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(stats.peak_used_bytes, stats.used_bytes);
        assert_eq!(stats.peak_reserved_bytes, stats.reserved_bytes);
    }

    #[test]
    #[cfg(feature = "aarm-telemetry")]
    fn aarm_telemetry_totals_are_derived_from_both_regions() {
        let mut context = ExecutionContext::with_stats();
        aster_rt_object_new(&raw mut context, 32);
        let mark = context.mark_temporary();
        context.allocate_temporary(64, 8);

        let telemetry = context
            .aarm_memory_telemetry()
            .expect("statistics mode enables telemetry");
        assert_eq!(telemetry.requested_bytes, 32);
        assert_eq!(telemetry.governor, None);
        assert_eq!(telemetry.persistent.live_used_bytes, 32);
        assert_eq!(telemetry.temporary.live_used_bytes, 64);
        assert_eq!(telemetry.total.live_used_bytes, 96);
        assert_eq!(
            telemetry.total.arena_capacity_bytes,
            telemetry.temporary.arena_capacity_bytes + telemetry.persistent.arena_capacity_bytes
        );
        assert_eq!(
            telemetry.total.virtual_extent_bytes,
            sum_known_u64(
                telemetry.temporary.virtual_extent_bytes,
                telemetry.persistent.virtual_extent_bytes,
            )
        );
        assert_eq!(
            telemetry.total.backing_retained_bytes,
            sum_known_u64(
                telemetry.temporary.backing_retained_bytes,
                telemetry.persistent.backing_retained_bytes,
            )
        );
        assert_eq!(
            telemetry.total.backing_discarded_bytes,
            sum_known_u64(
                telemetry.temporary.backing_discarded_bytes,
                telemetry.persistent.backing_discarded_bytes,
            )
        );
        assert_eq!(
            telemetry.total.active_page_capacity_bytes,
            telemetry.temporary.active_page_capacity_bytes
                + telemetry.persistent.active_page_capacity_bytes
        );
        assert_eq!(
            telemetry.total.inactive_page_capacity_bytes,
            telemetry.temporary.inactive_page_capacity_bytes
                + telemetry.persistent.inactive_page_capacity_bytes
        );
        assert_eq!(
            telemetry.total.page_count,
            telemetry.temporary.page_count + telemetry.persistent.page_count
        );
        assert_eq!(telemetry.total.peak_live_used_bytes, 96);

        context.rewind_temporary(mark);
        let telemetry = context
            .aarm_memory_telemetry()
            .expect("statistics mode enables telemetry");
        assert_eq!(telemetry.temporary.live_used_bytes, 0);
        assert_eq!(telemetry.persistent.live_used_bytes, 32);
        assert_eq!(telemetry.total.live_used_bytes, 32);
        assert_eq!(telemetry.temporary.events.rewind_events, 1);
        assert_eq!(telemetry.temporary.events.rewound_bytes, 64);
        assert_eq!(
            telemetry.temporary.last_rewind,
            Some(AarmRewindTelemetry {
                live_used_bytes_before: 64,
                live_used_bytes_after: 0,
                arena_capacity_bytes_before: MIN_PAGE_SIZE as u64,
                arena_capacity_bytes_after: MIN_PAGE_SIZE as u64,
                active_page_capacity_bytes_after: 0,
                inactive_page_capacity_bytes_after: MIN_PAGE_SIZE as u64,
            })
        );
        assert_eq!(telemetry.total.peak_live_used_bytes, 96);
    }

    #[test]
    #[cfg(feature = "aarm-telemetry")]
    fn aarm_telemetry_requires_statistics_mode() {
        assert!(ExecutionContext::new().aarm_memory_telemetry().is_none());

        let mut context = ExecutionContext::with_stats();
        aster_rt_object_new(&raw mut context, 32);
        let telemetry = context
            .aarm_memory_telemetry()
            .expect("statistics mode enables telemetry");
        assert_eq!(telemetry.requested_bytes, 32);
        assert_eq!(
            telemetry.total.live_used_bytes,
            telemetry.temporary.live_used_bytes + telemetry.persistent.live_used_bytes
        );
        assert_eq!(
            telemetry.total.arena_capacity_bytes,
            telemetry.temporary.arena_capacity_bytes + telemetry.persistent.arena_capacity_bytes
        );
    }

    #[test]
    #[cfg(feature = "aarm-telemetry")]
    fn aarm_limit_denial_does_not_increase_live_or_capacity_metrics() {
        let mut context = ExecutionContext::with_options(true, MIN_PAGE_SIZE);
        let request = i32::try_from(MIN_PAGE_SIZE + 1).expect("test request fits i32");
        let pointer = aster_rt_object_new(&raw mut context, request);
        assert!(pointer.is_null());

        let telemetry = context
            .aarm_memory_telemetry()
            .expect("statistics mode enables telemetry");
        assert_eq!(telemetry.total.live_used_bytes, 0);
        assert_eq!(telemetry.total.arena_capacity_bytes, 0);
        assert_eq!(telemetry.total.peak_live_used_bytes, 0);
        assert_eq!(telemetry.total.peak_arena_capacity_bytes, 0);
        assert_eq!(telemetry.total.events.allocation_limit_denials, 1);
        assert_eq!(context.memory_stats(), &MemoryStats::default());
    }

    #[test]
    #[cfg(feature = "aarm-telemetry")]
    fn governed_context_charges_only_fresh_page_capacity() {
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE));
        let mut context = ExecutionContext::with_memory_governor(Arc::clone(&governor));

        assert!(!aster_rt_object_new(&raw mut context, 16).is_null());
        let after_fresh = governor.telemetry();
        assert_eq!(after_fresh.current_capacity_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(after_fresh.grant_events, 1);

        assert!(!aster_rt_object_new(&raw mut context, 16).is_null());
        let after_fast = governor.telemetry();
        assert_eq!(after_fast.current_capacity_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(after_fast.grant_events, 1);
        let telemetry = context
            .aarm_memory_telemetry()
            .expect("governed contexts collect AARM telemetry");
        assert_eq!(telemetry.total.events.slow_path_allocations, 1);
        assert_eq!(telemetry.total.events.active_page_fast_path_allocations, 1);
        assert_eq!(telemetry.governor, Some(after_fast));

        drop(context);
        let after_drop = governor.telemetry();
        assert_eq!(after_drop.current_capacity_bytes, 0);
        assert_eq!(after_drop.release_events, 1);
    }

    #[test]
    #[cfg(feature = "aarm-telemetry")]
    fn temporary_rewind_and_reuse_keep_retained_capacity_charged() {
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE));
        let mut context = ExecutionContext::with_memory_governor(Arc::clone(&governor));

        let first_mark = context.mark_temporary();
        assert!(!context.allocate_temporary(64, 8).is_null());
        context.rewind_temporary(first_mark);
        let after_rewind = governor.telemetry();
        assert_eq!(after_rewind.current_capacity_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(after_rewind.grant_events, 1);
        assert_eq!(after_rewind.release_events, 0);

        let second_mark = context.mark_temporary();
        assert!(!context.allocate_temporary(64, 8).is_null());
        context.rewind_temporary(second_mark);
        let after_reuse = governor.telemetry();
        assert_eq!(after_reuse.current_capacity_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(after_reuse.grant_events, 1);
        let telemetry = context
            .aarm_memory_telemetry()
            .expect("governed contexts collect AARM telemetry");
        assert_eq!(telemetry.temporary.events.inactive_page_reuse_events, 1);
        assert_eq!(
            telemetry.temporary.inactive_page_capacity_bytes,
            MIN_PAGE_SIZE as u64
        );

        drop(context);
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }

    #[test]
    #[cfg(feature = "aarm-telemetry")]
    fn persistent_and_temporary_arenas_share_one_governor() {
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE * 2));
        let mut context = ExecutionContext::with_memory_governor(Arc::clone(&governor));
        assert!(!aster_rt_object_new(&raw mut context, 64).is_null());
        let mark = context.mark_temporary();
        assert!(!context.allocate_temporary(64, 8).is_null());

        let telemetry = context
            .aarm_memory_telemetry()
            .expect("governed contexts collect AARM telemetry");
        assert_eq!(
            telemetry.persistent.arena_capacity_bytes,
            MIN_PAGE_SIZE as u64
        );
        assert_eq!(
            telemetry.temporary.arena_capacity_bytes,
            MIN_PAGE_SIZE as u64
        );
        assert_eq!(
            telemetry.total.arena_capacity_bytes,
            (MIN_PAGE_SIZE * 2) as u64
        );
        assert_eq!(
            telemetry
                .governor
                .expect("governor telemetry is present")
                .current_capacity_bytes,
            telemetry.total.arena_capacity_bytes
        );

        context.rewind_temporary(mark);
        assert_eq!(
            governor.telemetry().current_capacity_bytes,
            (MIN_PAGE_SIZE * 2) as u64
        );
        drop(context);
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }

    #[test]
    #[cfg(feature = "aarm-telemetry")]
    fn shared_denial_is_controlled_and_teardown_makes_capacity_available() {
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE));
        let mut first = ExecutionContext::with_memory_governor(Arc::clone(&governor));
        let mut second = ExecutionContext::with_memory_governor(Arc::clone(&governor));
        assert!(!aster_rt_object_new(&raw mut first, 1).is_null());

        assert!(aster_rt_object_new(&raw mut second, 1).is_null());
        assert_eq!(
            second.take_error().as_deref(),
            Some("allocation exceeds the shared execution memory budget of 4096 bytes")
        );
        let denied = governor.telemetry();
        assert_eq!(denied.current_capacity_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(denied.denial_events, 1);
        assert!(denied.current_capacity_bytes <= denied.hard_limit_bytes);
        let second_telemetry = second
            .aarm_memory_telemetry()
            .expect("governed contexts collect AARM telemetry");
        assert_eq!(second_telemetry.total.arena_capacity_bytes, 0);
        assert_eq!(
            second_telemetry.total.events.fresh_regular_page_allocations,
            0
        );

        drop(first);
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
        assert!(!aster_rt_object_new(&raw mut second, 1).is_null());
        let reused_budget = governor.telemetry();
        assert_eq!(reused_budget.current_capacity_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(reused_budget.grant_events, 2);
        assert_eq!(reused_budget.release_events, 1);
        assert_eq!(
            reused_budget.granted_bytes_cumulative - reused_budget.released_bytes_cumulative,
            reused_budget.current_capacity_bytes
        );

        drop(second);
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }

    #[test]
    fn local_limit_is_checked_before_the_shared_governor() {
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE * 4));
        let mut context = ExecutionContext::with_options_and_governor(
            true,
            MIN_PAGE_SIZE,
            Some(Arc::clone(&governor)),
        );
        assert!(
            aster_rt_object_new(
                &raw mut context,
                i32::try_from(MIN_PAGE_SIZE + 1).expect("test size fits i32")
            )
            .is_null()
        );
        assert_eq!(
            context.take_error().as_deref(),
            Some("allocation exceeds the execution memory limit of 4096 bytes")
        );
        let telemetry = governor.telemetry();
        assert_eq!(telemetry.current_capacity_bytes, 0);
        assert_eq!(telemetry.grant_events, 0);
        assert_eq!(telemetry.denial_events, 0);
    }

    #[test]
    fn governed_local_budget_is_exact_at_fresh_page_boundaries() {
        let below_governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE * 4));
        let mut below =
            ExecutionContext::with_memory_budget(Arc::clone(&below_governor), MIN_PAGE_SIZE - 1);
        assert!(aster_rt_object_new(&raw mut below, 1).is_null());
        assert_eq!(below_governor.telemetry().current_capacity_bytes, 0);

        let exact_governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE * 4));
        let mut exact =
            ExecutionContext::with_memory_budget(Arc::clone(&exact_governor), MIN_PAGE_SIZE);
        assert!(!aster_rt_object_new(&raw mut exact, 1).is_null());
        assert_eq!(
            exact_governor.telemetry().current_capacity_bytes,
            MIN_PAGE_SIZE as u64
        );

        let above_governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE * 4));
        let mut above =
            ExecutionContext::with_memory_budget(Arc::clone(&above_governor), MIN_PAGE_SIZE + 1);
        assert!(!aster_rt_object_new(&raw mut above, 1).is_null());
        assert_eq!(
            above_governor.telemetry().current_capacity_bytes,
            MIN_PAGE_SIZE as u64
        );

        drop((exact, above));
        assert_eq!(exact_governor.telemetry().current_capacity_bytes, 0);
        assert_eq!(above_governor.telemetry().current_capacity_bytes, 0);
    }

    #[test]
    fn governed_local_budget_never_weakens_the_one_gibibyte_safety_ceiling() {
        let governor = Arc::new(MemoryGovernor::new(usize::MAX));
        let context = ExecutionContext::with_memory_budget(governor, usize::MAX);
        assert_eq!(
            context.allocation_limit_bytes,
            EXECUTION_ALLOCATION_LIMIT_BYTES
        );
    }

    #[test]
    fn governed_local_budget_caps_multiple_page_growth_before_global_admission() {
        let local_limit = MIN_PAGE_SIZE * 3;
        let governor = Arc::new(MemoryGovernor::new(local_limit * 4));
        let mut context = ExecutionContext::with_memory_budget(Arc::clone(&governor), local_limit);

        assert!(!aster_rt_object_new(&raw mut context, 1).is_null());
        assert!(
            !aster_rt_object_new(
                &raw mut context,
                i32::try_from(MIN_PAGE_SIZE).expect("page size fits i32")
            )
            .is_null()
        );
        let admitted = governor.telemetry();
        assert_eq!(admitted.current_capacity_bytes, local_limit as u64);
        assert_eq!(admitted.grant_events, 2);

        assert!(
            aster_rt_object_new(
                &raw mut context,
                i32::try_from(MIN_PAGE_SIZE * 2).expect("test allocation fits i32")
            )
            .is_null()
        );
        let denied = governor.telemetry();
        assert_eq!(denied.current_capacity_bytes, local_limit as u64);
        assert_eq!(denied.grant_events, 2);
        assert_eq!(denied.denial_events, 0);
    }

    #[test]
    fn shared_governor_enforcement_does_not_depend_on_telemetry_feature() {
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE));
        let mut first = ExecutionContext::with_memory_governor(Arc::clone(&governor));
        let mut second = ExecutionContext::with_memory_governor(Arc::clone(&governor));
        assert!(!aster_rt_object_new(&raw mut first, 1).is_null());
        assert!(aster_rt_object_new(&raw mut second, 1).is_null());
        assert_eq!(
            second.take_error().as_deref(),
            Some("allocation exceeds the shared execution memory budget of 4096 bytes")
        );
        assert_eq!(
            governor.telemetry().current_capacity_bytes,
            MIN_PAGE_SIZE as u64
        );
        drop((first, second));
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }

    #[test]
    fn independent_contexts_have_independent_arenas() {
        let mut a = ExecutionContext::with_stats();
        let mut b = ExecutionContext::with_stats();
        aster_rt_object_new(&raw mut a, 32);
        aster_rt_object_new(&raw mut b, 64);
        assert_eq!(a.memory_stats().requested_bytes, 32);
        assert_eq!(b.memory_stats().requested_bytes, 64);
    }

    #[test]
    fn many_object_allocations_cross_multiple_pages() {
        let mut context = ExecutionContext::with_stats();
        let ctx = &raw mut context;
        for _ in 0..10_000 {
            let p = aster_rt_object_new(ctx, 32);
            assert!(!p.is_null());
        }
        let stats = context.memory_stats();
        assert_eq!(stats.total_allocations, 10_000);
        assert_eq!(stats.object_allocations, 10_000);
        assert_eq!(stats.requested_bytes, 320_000);
        assert!(stats.used_bytes >= 320_000);
        assert!(stats.reserved_bytes >= stats.used_bytes);
        // With 64 KiB pages and 32-byte objects, we should have far fewer
        // pages than individual allocations.
        assert!(stats.reserved_bytes <= stats.used_bytes * 2);
    }

    #[test]
    fn fresh_context_owns_two_empty_arenas() {
        let context = ExecutionContext::new();
        let persistent = context.arena.metrics();
        let temporary = context.temporary_arena.metrics();

        assert_eq!(persistent.used_bytes, 0);
        assert_eq!(persistent.reserved_bytes, 0);
        assert_eq!(temporary.used_bytes, 0);
        assert_eq!(temporary.reserved_bytes, 0);
    }

    #[test]
    fn normal_allocations_use_only_the_persistent_arena() {
        let mut context = ExecutionContext::with_stats();
        aster_rt_object_new(&raw mut context, 32);

        let persistent = context.arena.metrics();
        let temporary = context.temporary_arena.metrics();
        let stats = context.memory_stats();

        assert_eq!(persistent.used_bytes, 32);
        assert_eq!(temporary.used_bytes, 0);
        assert_eq!(temporary.reserved_bytes, 0);
        assert_eq!(stats.used_bytes, 32);
        assert_eq!(stats.reserved_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(stats.total_allocations, 1);
        assert_eq!(stats.object_allocations, 1);
        assert_eq!(stats.requested_bytes, 32);
    }

    #[test]
    fn temporary_allocation_updates_combined_usage_without_logical_counts() {
        let mut context = ExecutionContext::with_stats();
        let pointer = context.allocate_temporary(32, 8);

        assert!(!pointer.is_null());
        assert_eq!(context.arena.metrics().used_bytes, 0);
        assert_eq!(context.temporary_arena.metrics().used_bytes, 32);

        let stats = context.memory_stats();
        assert_eq!(stats.used_bytes, 32);
        assert_eq!(stats.reserved_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(stats.peak_used_bytes, 32);
        assert_eq!(stats.peak_reserved_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(stats.total_allocations, 0);
        assert_eq!(stats.requested_bytes, 0);
    }

    #[test]
    fn temporary_rewind_restores_usage_but_keeps_capacity_and_peak() {
        let mut context = ExecutionContext::with_stats();
        let mark = context.mark_temporary();
        context.allocate_temporary(1024, 8);

        let reserved = context.memory_stats().reserved_bytes;
        let peak_used = context.memory_stats().peak_used_bytes;
        context.rewind_temporary(mark);

        let stats = context.memory_stats();
        assert_eq!(context.temporary_arena.metrics().used_bytes, 0);
        assert_eq!(stats.used_bytes, 0);
        assert_eq!(stats.reserved_bytes, reserved);
        assert_eq!(stats.peak_used_bytes, peak_used);
        assert_eq!(peak_used, 1024);
    }

    #[test]
    fn temporary_rewind_preserves_data_allocated_before_the_mark() {
        let mut context = ExecutionContext::with_stats();
        let persistent_temporary = context.allocate_temporary(8, 8);
        // SAFETY: the pointer refers to eight live bytes in the temporary arena.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(persistent_temporary as *mut u64, 0xCAFE_BABE);
        }

        let mark = context.mark_temporary();
        context.allocate_temporary(256, 8);
        context.rewind_temporary(mark);

        // SAFETY: this allocation predates the mark and remains active.
        #[allow(unsafe_code)]
        let value = unsafe { std::ptr::read(persistent_temporary as *const u64) };
        assert_eq!(value, 0xCAFE_BABE);
        assert_eq!(context.memory_stats().used_bytes, 8);
    }

    #[test]
    fn rewound_temporary_memory_is_zeroed_when_reused() {
        let mut context = ExecutionContext::with_stats();
        let mark = context.mark_temporary();
        let old = context.allocate_temporary(64, 8);
        // SAFETY: `old` points to 64 live bytes in the temporary arena.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write_bytes(old, 0xAB, 64);
        }

        context.rewind_temporary(mark);
        let reused = context.allocate_temporary(64, 8);

        assert_eq!(reused, old);
        // SAFETY: `reused` is a new live allocation of 64 bytes.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(reused, 64) };
        assert!(bytes.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn temporary_capacity_is_reused_after_rewind() {
        let mut context = ExecutionContext::with_stats();
        let mark = context.mark_temporary();
        context.allocate_temporary(64 * 1024, 8);
        let reserved = context.temporary_arena.metrics().reserved_bytes;

        context.rewind_temporary(mark);
        context.allocate_temporary(64, 8);

        assert_eq!(context.temporary_arena.metrics().reserved_bytes, reserved);
        assert_eq!(context.memory_stats().reserved_bytes, reserved as u64);
    }

    #[test]
    fn combined_peak_is_the_maximum_simultaneous_usage() {
        let mut context = ExecutionContext::with_stats();
        let mark = context.mark_temporary();
        context.allocate_temporary(64 * 1024, 8);
        assert_eq!(context.memory_stats().peak_used_bytes, 64 * 1024);

        context.rewind_temporary(mark);
        aster_rt_object_new(&raw mut context, 32 * 1024);

        let stats = context.memory_stats();
        assert_eq!(stats.used_bytes, 32 * 1024);
        assert_eq!(stats.peak_used_bytes, 64 * 1024);
        assert_ne!(stats.peak_used_bytes, 96 * 1024);
    }

    #[test]
    fn nested_temporary_marks_rewind_in_lifo_order() {
        let mut context = ExecutionContext::with_stats();
        let outer = context.mark_temporary();
        context.allocate_temporary(8, 8);
        let inner = context.mark_temporary();
        context.allocate_temporary(16, 8);

        context.rewind_temporary(inner);
        assert_eq!(context.memory_stats().used_bytes, 8);

        context.rewind_temporary(outer);
        assert_eq!(context.memory_stats().used_bytes, 0);
    }

    #[test]
    #[should_panic(expected = "arena mark belongs to a different arena")]
    fn temporary_mark_from_another_context_is_rejected() {
        let mut first = ExecutionContext::new();
        let mut second = ExecutionContext::new();
        let mark = first.mark_temporary();

        second.rewind_temporary(mark);
    }

    #[test]
    #[should_panic(expected = "arena mark belongs to a different arena")]
    fn persistent_arena_mark_is_rejected_by_temporary_rewind() {
        let mut context = ExecutionContext::new();
        let wrong_mark = TemporaryArenaMark(context.arena.mark());

        context.rewind_temporary(wrong_mark);
    }

    #[test]
    fn temporary_object_scope_rewinds_usage_and_preserves_logical_stats() {
        let mut context = ExecutionContext::with_stats();
        let pointer = &raw mut context;

        aster_rt_temporary_scope_enter(pointer);
        let object = aster_rt_object_new_temporary(pointer, 32);
        assert!(!object.is_null());
        assert_eq!(context.memory_stats().used_bytes, 32);

        aster_rt_temporary_scope_leave(pointer);

        let stats = context.memory_stats();
        assert_eq!(stats.total_allocations, 1);
        assert_eq!(stats.object_allocations, 1);
        assert_eq!(stats.requested_bytes, 32);
        assert_eq!(stats.used_bytes, 0);
        assert_eq!(stats.reserved_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(stats.peak_used_bytes, 32);
    }

    #[test]
    fn nested_runtime_temporary_scopes_preserve_outer_objects() {
        let mut context = ExecutionContext::with_stats();
        let pointer = &raw mut context;

        aster_rt_temporary_scope_enter(pointer);
        let outer = aster_rt_object_new_temporary(pointer, 8);
        assert!(!outer.is_null());
        // SAFETY: `outer` points to eight live bytes in the active outer scope.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(outer.cast::<u64>(), 42);
        }

        aster_rt_temporary_scope_enter(pointer);
        let inner = aster_rt_object_new_temporary(pointer, 16);
        assert!(!inner.is_null());
        aster_rt_temporary_scope_leave(pointer);

        // SAFETY: leaving the inner scope cannot rewind the outer allocation.
        #[allow(unsafe_code)]
        let value = unsafe { std::ptr::read(outer.cast::<u64>()) };
        assert_eq!(value, 42);
        assert_eq!(context.memory_stats().used_bytes, 8);

        aster_rt_temporary_scope_leave(pointer);
        assert_eq!(context.memory_stats().used_bytes, 0);
        assert_eq!(context.memory_stats().object_allocations, 2);
    }

    #[test]
    fn fine_temporary_subregion_rewinds_without_changing_function_scope_depth() {
        let mut context = ExecutionContext::with_stats();
        let pointer = &raw mut context;

        aster_rt_temporary_scope_enter(pointer);
        let outer = aster_rt_object_new_temporary(pointer, 8);
        assert!(!outer.is_null());
        // SAFETY: `outer` points to eight live bytes owned by the enclosing
        // function Temporary scope.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(outer.cast::<u64>(), 42);
        }
        assert_eq!(context.temporary_scopes.len(), 1);

        aster_rt_temporary_subregion_enter(pointer);
        assert_eq!(context.temporary_scopes.len(), 1);
        assert!(context.fine_temporary_subregion.is_some());
        let list = context.allocate_list_in_region(4, 4, 1, ListRegion::Temporary);
        let dictionary = context.allocate_dictionary_in_region(
            DictionaryKeyKind::Int,
            4,
            4,
            1,
            4,
            4,
            2,
            ListRegion::Temporary,
        );
        let builder = context.allocate_string_builder_in_region(ListRegion::Temporary);
        assert!(!list.is_null());
        assert!(!dictionary.is_null());
        assert!(!builder.is_null());
        // SAFETY: all three headers are live, context-owned allocations. The
        // test reads only their initialized private semantic-depth fields.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!((*list).birth_scope_depth, 1);
            assert_eq!((*dictionary).birth_scope_depth, 1);
            assert_eq!((*builder).birth_scope_depth, 1);
        }
        let inner = aster_rt_object_new_temporary(pointer, 32);
        assert!(!inner.is_null());
        assert!(context.memory_stats().used_bytes > 40);

        aster_rt_temporary_subregion_exit(pointer);
        assert_eq!(context.temporary_scopes.len(), 1);
        assert!(context.fine_temporary_subregion.is_none());
        assert_eq!(context.memory_stats().used_bytes, 8);
        // SAFETY: fine rewind returns only to the newer subregion mark, so the
        // enclosing function-scope allocation remains live and unmoved.
        #[allow(unsafe_code)]
        let value = unsafe { std::ptr::read(outer.cast::<u64>()) };
        assert_eq!(value, 42);

        aster_rt_temporary_scope_leave(pointer);
        assert_eq!(context.memory_stats().used_bytes, 0);
        assert!(context.take_error().is_none());
    }

    #[test]
    #[ignore = "manual release-only AARM checkpoint cost measurement"]
    fn benchmark_fine_checkpoint_cost_components() {
        use std::hint::black_box;

        #[allow(clippy::cast_precision_loss)]
        fn median_ns(mut samples: Vec<u128>, iterations: usize) -> f64 {
            samples.sort_unstable();
            samples[samples.len() / 2] as f64 / iterations as f64
        }

        fn measure(samples: usize, mut run: impl FnMut()) -> Vec<u128> {
            (0..samples)
                .map(|_| {
                    let started = Instant::now();
                    run();
                    started.elapsed().as_nanos()
                })
                .collect()
        }

        const SAMPLES: usize = 7;
        let iterations = std::env::var("ASTER_AARM_CHECKPOINT_ITERATIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1_000_000);
        let multi_page_iterations = (iterations / 1_000).max(1_000);

        let baseline = median_ns(
            measure(SAMPLES, || {
                for index in 0..iterations {
                    black_box(index);
                }
            }),
            iterations,
        );

        let mut direct = ExecutionContext::new();
        direct.enter_temporary_scope();
        let direct_mark_rewind = median_ns(
            measure(SAMPLES, || {
                for _ in 0..iterations {
                    let mark = direct.mark_temporary();
                    direct.rewind_temporary(mark);
                }
            }),
            iterations,
        );
        direct.leave_temporary_scope();

        let mut abi = ExecutionContext::new();
        let abi_pointer = &raw mut abi;
        aster_rt_temporary_scope_enter(abi_pointer);
        let abi_empty = median_ns(
            measure(SAMPLES, || {
                for _ in 0..iterations {
                    aster_rt_temporary_subregion_enter(black_box(abi_pointer));
                    aster_rt_temporary_subregion_exit(black_box(abi_pointer));
                }
            }),
            iterations,
        );
        aster_rt_temporary_scope_leave(abi_pointer);

        let mut same_page = ExecutionContext::new();
        let same_page_pointer = &raw mut same_page;
        aster_rt_temporary_scope_enter(same_page_pointer);
        let same_page_32 = median_ns(
            measure(SAMPLES, || {
                for _ in 0..iterations {
                    aster_rt_temporary_subregion_enter(same_page_pointer);
                    black_box(aster_rt_object_new_temporary(same_page_pointer, 32));
                    aster_rt_temporary_subregion_exit(same_page_pointer);
                }
            }),
            iterations,
        );
        aster_rt_temporary_scope_leave(same_page_pointer);

        let mut multi_page = ExecutionContext::new();
        let multi_page_pointer = &raw mut multi_page;
        aster_rt_temporary_scope_enter(multi_page_pointer);
        let multi_page_8192 = median_ns(
            measure(SAMPLES, || {
                for _ in 0..multi_page_iterations {
                    aster_rt_temporary_subregion_enter(multi_page_pointer);
                    black_box(aster_rt_object_new_temporary(
                        multi_page_pointer,
                        i32::try_from(MIN_PAGE_SIZE * 2).expect("benchmark size fits i32"),
                    ));
                    aster_rt_temporary_subregion_exit(multi_page_pointer);
                }
            }),
            multi_page_iterations,
        );
        aster_rt_temporary_scope_leave(multi_page_pointer);

        println!(
            "AARM checkpoint ns/iteration: baseline={baseline:.2} direct_mark_rewind={direct_mark_rewind:.2} abi_empty={abi_empty:.2} same_page_32={same_page_32:.2} multi_page_8192={multi_page_8192:.2} telemetry={}",
            cfg!(feature = "aarm-telemetry")
        );
    }

    #[test]
    fn fine_subregion_error_cleanup_preserves_first_error() {
        let mut preexisting = ExecutionContext::new();
        let pointer = &raw mut preexisting;
        aster_rt_temporary_scope_enter(pointer);
        preexisting.fail("earlier failure");
        aster_rt_temporary_subregion_enter(pointer);
        assert!(preexisting.fine_temporary_subregion.is_none());
        aster_rt_temporary_scope_leave(pointer);
        assert_eq!(preexisting.take_error().as_deref(), Some("earlier failure"));

        let mut active = ExecutionContext::with_stats();
        let pointer = &raw mut active;
        aster_rt_temporary_scope_enter(pointer);
        aster_rt_temporary_subregion_enter(pointer);
        assert!(!aster_rt_object_new_temporary(pointer, 32).is_null());
        active.fail("allocation failed");

        // Fine cleanup must still rewind while first-error-wins is active.
        aster_rt_temporary_subregion_exit(pointer);
        assert!(active.fine_temporary_subregion.is_none());
        assert_eq!(active.memory_stats().used_bytes, 0);
        aster_rt_temporary_scope_leave(pointer);
        assert_eq!(active.take_error().as_deref(), Some("allocation failed"));
    }

    #[test]
    fn malformed_fine_subregion_ordering_is_controlled() {
        let mut context = ExecutionContext::new();
        let pointer = &raw mut context;

        aster_rt_temporary_subregion_enter(pointer);
        assert_eq!(
            context.take_error().as_deref(),
            Some("temporary subregion enter requires an active temporary scope")
        );
        assert!(context.fine_temporary_subregion.is_none());

        aster_rt_temporary_subregion_exit(pointer);
        assert_eq!(
            context.take_error().as_deref(),
            Some("temporary subregion exit has no matching enter")
        );

        aster_rt_temporary_scope_enter(pointer);
        aster_rt_temporary_subregion_enter(pointer);
        aster_rt_temporary_subregion_enter(pointer);
        assert_eq!(
            context.take_error().as_deref(),
            Some("temporary subregion enter has an active subregion")
        );
        assert_eq!(context.temporary_scopes.len(), 1);
        assert!(context.fine_temporary_subregion.is_some());

        aster_rt_temporary_subregion_exit(pointer);
        aster_rt_temporary_scope_leave(pointer);
        assert!(context.temporary_scopes.is_empty());
        assert!(context.fine_temporary_subregion.is_none());

        aster_rt_temporary_subregion_exit(pointer);
        assert_eq!(
            context.take_error().as_deref(),
            Some("temporary subregion exit has no matching enter")
        );
        assert!(context.temporary_scopes.is_empty());
        assert!(context.fine_temporary_subregion.is_none());

        aster_rt_temporary_subregion_enter(std::ptr::null_mut());
        aster_rt_temporary_subregion_exit(std::ptr::null_mut());
    }

    #[test]
    fn function_scope_transitions_cannot_cross_an_active_fine_subregion() {
        let mut enter = ExecutionContext::new();
        let pointer = &raw mut enter;
        aster_rt_temporary_scope_enter(pointer);
        aster_rt_temporary_subregion_enter(pointer);
        aster_rt_temporary_scope_enter(pointer);
        assert_eq!(
            enter.take_error().as_deref(),
            Some("temporary scope enter cannot occur while a temporary subregion is active")
        );
        assert_eq!(enter.temporary_scopes.len(), 1);
        assert!(enter.fine_temporary_subregion.is_some());
        aster_rt_temporary_subregion_exit(pointer);
        aster_rt_temporary_scope_leave(pointer);

        let mut leave = ExecutionContext::new();
        let pointer = &raw mut leave;
        aster_rt_temporary_scope_enter(pointer);
        aster_rt_temporary_subregion_enter(pointer);
        aster_rt_temporary_scope_leave(pointer);
        assert_eq!(
            leave.take_error().as_deref(),
            Some("temporary scope leave cannot occur while a temporary subregion is active")
        );
        assert_eq!(leave.temporary_scopes.len(), 1);
        assert!(leave.fine_temporary_subregion.is_some());
        aster_rt_temporary_subregion_exit(pointer);
        aster_rt_temporary_scope_leave(pointer);
        assert!(leave.temporary_scopes.is_empty());
        assert!(leave.fine_temporary_subregion.is_none());
    }

    #[test]
    fn unmatched_runtime_temporary_scope_leave_is_controlled() {
        let mut context = ExecutionContext::new();

        aster_rt_temporary_scope_leave(&raw mut context);

        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("no matching enter"))
        );
    }

    #[test]
    fn call_depth_limit_is_controlled_and_balances_during_unwind() {
        let mut context = ExecutionContext::new();
        let pointer = &raw mut context;

        for _ in 0..ASTER_CALL_DEPTH_LIMIT {
            assert_eq!(aster_rt_call_enter(pointer), 1);
        }
        assert_eq!(aster_rt_call_enter(pointer), 0);
        assert_eq!(
            context.take_error().as_deref(),
            Some("ASTER call depth exceeds the supported limit of 1024")
        );

        for _ in 0..ASTER_CALL_DEPTH_LIMIT {
            aster_rt_call_leave(pointer);
        }
        assert_eq!(context.call_depth, 0);
        assert_eq!(aster_rt_call_enter(pointer), 1);
        aster_rt_call_leave(pointer);
        assert_eq!(context.call_depth, 0);
        assert_eq!(context.take_error(), None);
    }

    #[test]
    fn call_depth_failure_is_first_error_wins_and_context_local() {
        let mut first = ExecutionContext::new();
        let first_pointer = &raw mut first;
        for _ in 0..ASTER_CALL_DEPTH_LIMIT {
            assert_eq!(aster_rt_call_enter(first_pointer), 1);
        }
        assert_eq!(aster_rt_call_enter(first_pointer), 0);
        first.fail("later error");
        assert_eq!(aster_rt_has_error(first_pointer), 1);

        let mut second = ExecutionContext::new();
        let second_pointer = &raw mut second;
        assert_eq!(aster_rt_call_enter(second_pointer), 1);
        aster_rt_call_leave(second_pointer);
        assert_eq!(aster_rt_has_error(second_pointer), 0);
        assert_eq!(second.take_error(), None);

        assert_eq!(
            first.take_error().as_deref(),
            Some("ASTER call depth exceeds the supported limit of 1024")
        );
        for _ in 0..ASTER_CALL_DEPTH_LIMIT {
            aster_rt_call_leave(first_pointer);
        }
        assert_eq!(first.call_depth, 0);
    }

    #[test]
    fn temporary_object_without_scope_is_rejected_without_panicking() {
        let mut context = ExecutionContext::new();

        let object = aster_rt_object_new_temporary(&raw mut context, 16);

        assert!(object.is_null());
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("active temporary scope"))
        );
    }

    #[test]
    fn temporary_array_scope_rewinds_header_and_data_together() {
        let mut context = ExecutionContext::with_stats();
        let pointer = &raw mut context;

        aster_rt_temporary_scope_enter(pointer);
        let array = aster_rt_array_new_temporary(pointer, 2, 4);
        assert!(!array.is_null());
        assert_eq!(aster_rt_array_length(pointer, array), 2);
        let element = aster_rt_array_element(pointer, array, 1);
        assert!(!element.is_null());
        // SAFETY: the element points to four live bytes in the active scope.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(element.cast::<u32>(), 42);
            assert_eq!(std::ptr::read(element.cast::<u32>()), 42);
        }
        assert!(context.memory_stats().used_bytes > 0);

        aster_rt_temporary_scope_leave(pointer);

        let stats = context.memory_stats();
        assert_eq!(stats.total_allocations, 1);
        assert_eq!(stats.array_allocations, 1);
        assert_eq!(stats.used_bytes, 0);
        assert!(stats.peak_used_bytes > 0);
    }

    #[test]
    fn temporary_string_scope_rewinds_storage() {
        let mut context = ExecutionContext::with_stats();
        let pointer = &raw mut context;

        aster_rt_temporary_scope_enter(pointer);
        let string = context.allocate_temporary_string_parts(&["Aster"]);
        assert!(!string.is_null());
        // SAFETY: the string is live until the matching scope leave below.
        #[allow(unsafe_code)]
        let text = unsafe { crate::string::view(string) };
        assert_eq!(text, Some("Aster"));

        aster_rt_temporary_scope_leave(pointer);

        let stats = context.memory_stats();
        assert_eq!(stats.total_allocations, 1);
        assert_eq!(stats.string_allocations, 1);
        assert_eq!(stats.used_bytes, 0);
        assert!(stats.peak_used_bytes > 0);
    }

    #[test]
    fn temporary_array_and_string_require_an_active_scope() {
        let mut context = ExecutionContext::new();

        let array = aster_rt_array_new_temporary(&raw mut context, 1, 4);
        assert!(array.is_null());
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("temporary array allocation"))
        );

        let string = context.allocate_temporary_string_parts(&["Aster"]);
        assert!(string.is_null());
        assert!(
            context
                .take_error()
                .is_some_and(|error| error.contains("temporary string allocation"))
        );
    }
}
