//! Per-execution ownership and the array/list runtime ABI.

use std::mem::{align_of, size_of};
use std::ptr;

use crate::arena::{ArenaMark, PagedArena};
use crate::string::AsterStrHeader;

/// Stable array header visible to generated code only through runtime calls.
#[repr(C)]
pub struct AsterArray {
    data: *mut u8,
    length: i32,
    element_size: u32,
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
/// must reuse the exact same arena — never deduced later by comparing
/// pointer addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ListRegion {
    Persistent,
    Temporary,
}

/// Runtime-owned header for one `List<T>` instance. `T`'s content never lives
/// inline in the value that names the list (that value is always a pointer
/// to this header, exactly like `Array`/`Class`); this struct owns the
/// growable buffer instead.
///
/// Invariant: `data` is only ever dereferenced when `capacity > 0`. A freshly
/// allocated list has `capacity == 0`, and `data` is `null` in that case —
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
    region: ListRegion,
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

#[derive(Clone, Copy)]
enum AllocationCategory {
    Object,
    Array,
    String,
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
    temporary_scopes: Vec<TemporaryArenaMark>,
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
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionContext {
    #[must_use]
    pub fn new() -> Self {
        Self {
            arena: PagedArena::new(),
            temporary_arena: PagedArena::new(),
            temporary_scopes: Vec::new(),
            error: None,
            collect_stats: false,
            stats: MemoryStats::default(),
            task_runtime: None,
        }
    }

    #[must_use]
    pub fn with_stats() -> Self {
        Self {
            arena: PagedArena::new(),
            temporary_arena: PagedArena::new(),
            temporary_scopes: Vec::new(),
            error: None,
            collect_stats: true,
            stats: MemoryStats::default(),
            task_runtime: None,
        }
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    #[must_use]
    pub fn memory_stats(&self) -> &MemoryStats {
        &self.stats
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

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn allocate_temporary(&mut self, size: usize, align: usize) -> *mut u8 {
        let pointer = self.temporary_arena.alloc(size, align);
        self.refresh_memory_usage();
        pointer
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn rewind_temporary(&mut self, mark: TemporaryArenaMark) {
        self.temporary_arena.rewind(mark.0);
        self.refresh_memory_usage();
    }

    fn enter_temporary_scope(&mut self) {
        let mark = self.mark_temporary();
        self.temporary_scopes.push(mark);
    }

    fn leave_temporary_scope(&mut self) {
        let Some(mark) = self.temporary_scopes.pop() else {
            self.fail("temporary scope leave has no matching enter");
            return;
        };
        self.rewind_temporary(mark);
    }

    pub(crate) fn allocate_temporary_object(&mut self, size: u32) -> *mut u8 {
        if self.temporary_scopes.is_empty() {
            self.fail("temporary object allocation requires an active temporary scope");
            return ptr::null_mut();
        }
        let bytes = usize::try_from(size.max(1)).unwrap_or(1);
        let pointer = self.temporary_arena.alloc(bytes, 8);
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
        let bytes = usize::try_from(valid_length)
            .ok()
            .and_then(|length| length.checked_mul(valid_size as usize));
        let bytes = if let Some(bytes) = bytes {
            bytes.max(valid_size as usize)
        } else {
            self.fail("array allocation size exceeds the addressable range");
            valid_size as usize
        };

        let (data, header_ptr) = {
            let arena = if temporary {
                &mut self.temporary_arena
            } else {
                &mut self.arena
            };
            let data = arena.alloc(bytes, 8);
            #[allow(clippy::cast_ptr_alignment)]
            let header_ptr = arena
                .alloc(size_of::<AsterArray>(), align_of::<AsterArray>())
                .cast::<AsterArray>();
            (data, header_ptr)
        };

        // SAFETY: `header_ptr` points to zeroed, correctly aligned memory for
        // `AsterArray`. Header and data belong to the same selected arena and
        // therefore have exactly the same lifetime.
        #[allow(unsafe_code)]
        unsafe {
            (*header_ptr).data = data;
            (*header_ptr).length = valid_length;
            (*header_ptr).element_size = valid_size;
        }
        self.record_allocation(AllocationCategory::Array, bytes);
        header_ptr
    }

    fn allocate_array(&mut self, length: i32, element_size: u32) -> *mut AsterArray {
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
    /// reported through `self.fail` and returns a null pointer — never a
    /// panic, never a trap, never a partially written header (nothing is
    /// written to the header until every validation has passed). Capacity
    /// is not reserved ahead of time: growth is a future operation.
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
        if region == ListRegion::Temporary && self.temporary_scopes.is_empty() {
            self.fail("temporary list allocation requires an active temporary scope");
            return ptr::null_mut();
        }

        let header_ptr = {
            let arena = if region == ListRegion::Temporary {
                &mut self.temporary_arena
            } else {
                &mut self.arena
            };
            #[allow(clippy::cast_ptr_alignment)]
            arena
                .alloc(size_of::<AsterList>(), align_of::<AsterList>())
                .cast::<AsterList>()
        };

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
            (*header_ptr).region = region;
        }
        self.record_allocation(AllocationCategory::Object, size_of::<AsterList>());
        header_ptr
    }

    pub(crate) fn allocate_object(&mut self, size: u32) -> *mut u8 {
        let bytes = usize::try_from(size.max(1)).unwrap_or(1);
        let pointer = self.arena.alloc(bytes, 8);
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

        let pointer = {
            let arena = if temporary {
                &mut self.temporary_arena
            } else {
                &mut self.arena
            };
            arena.alloc(total_bytes, align_of::<AsterStrHeader>())
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
pub extern "C" fn aster_rt_list_length(
    context: *mut ExecutionContext,
    list: *const AsterList,
) -> i32 {
    if context.is_null() || list.is_null() {
        return 0;
    }
    // SAFETY: list headers are owned by the live context passed alongside it.
    #[allow(unsafe_code)]
    unsafe {
        (*list).length
    }
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
        assert_eq!(stats.reserved_bytes, 64 * 1024);
        assert_eq!(stats.peak_used_bytes, stats.used_bytes);
        assert_eq!(stats.peak_reserved_bytes, stats.reserved_bytes);
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
        assert_eq!(stats.reserved_bytes, 64 * 1024);
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
        assert_eq!(stats.reserved_bytes, 64 * 1024);
        assert_eq!(stats.peak_used_bytes, 32);
        assert_eq!(stats.peak_reserved_bytes, 64 * 1024);
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
        assert_eq!(stats.reserved_bytes, 64 * 1024);
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
