//! Per-execution ownership and the array runtime ABI.

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
/// Persistent allocations live in `arena`; a second paged arena is reserved
/// for future temporary allocations. Both arenas are released in bulk when
/// the context is dropped. No individual deallocation exists.
pub struct ExecutionContext {
    arena: PagedArena,
    temporary_arena: PagedArena,
    error: Option<String>,
    collect_stats: bool,
    stats: MemoryStats,
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
            error: None,
            collect_stats: false,
            stats: MemoryStats::default(),
        }
    }

    #[must_use]
    pub fn with_stats() -> Self {
        Self {
            arena: PagedArena::new(),
            temporary_arena: PagedArena::new(),
            error: None,
            collect_stats: true,
            stats: MemoryStats::default(),
        }
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    #[must_use]
    pub fn memory_stats(&self) -> &MemoryStats {
        &self.stats
    }

    pub(crate) fn fail(&mut self, message: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(message.into());
        }
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

    fn allocate_array(&mut self, length: i32, element_size: u32) -> *mut AsterArray {
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
        let data = self.arena.alloc(bytes, 8);
        #[allow(clippy::cast_ptr_alignment)]
        let header_ptr = self
            .arena
            .alloc(size_of::<AsterArray>(), align_of::<AsterArray>())
            .cast::<AsterArray>();
        // SAFETY: `header_ptr` points to zeroed, correctly aligned memory for
        // `AsterArray`. No Rust reference to arena memory is held across this
        // write. The data pointer was returned by a prior arena allocation and
        // remains stable.
        #[allow(unsafe_code)]
        unsafe {
            (*header_ptr).data = data;
            (*header_ptr).length = valid_length;
            (*header_ptr).element_size = valid_size;
        }
        self.record_allocation(AllocationCategory::Array, bytes);
        header_ptr
    }

    pub(crate) fn allocate_object(&mut self, size: u32) -> *mut u8 {
        let bytes = usize::try_from(size.max(1)).unwrap_or(1);
        let pointer = self.arena.alloc(bytes, 8);
        self.record_allocation(AllocationCategory::Object, bytes);
        pointer
    }

    pub(crate) fn allocate_string_parts(&mut self, parts: &[&str]) -> *const AsterStrHeader {
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
        let ptr = self.arena.alloc(total_bytes, align_of::<AsterStrHeader>());
        // SAFETY: `ptr` points to `total_bytes` of zeroed, correctly aligned
        // memory owned by the arena. No other reference to this region exists.
        // The slice is consumed before `record_allocation` borrows `self`.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts_mut(ptr, total_bytes) };
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
        ptr.cast::<AsterStrHeader>()
    }
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

#[cfg(test)]
#[allow(
    clippy::cast_ptr_alignment,
    clippy::ptr_as_ptr,
    clippy::ptr_cast_constness
)]
mod tests {
    use super::*;
    use crate::object::aster_rt_object_new;

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
}
