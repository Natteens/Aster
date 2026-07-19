//! Per-execution ownership and the array runtime ABI.

use std::ptr;

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

/// Owns every dynamic allocation made by one JIT invocation.
/// Dropping the context releases all individually boxed buffers together.
///
/// This is **not** a bump arena or region allocator. Each allocation
/// produces its own `Box<[u64]>`, and all boxes are freed when the
/// context is dropped. No individual deallocation exists.
#[derive(Default)]
pub struct ExecutionContext {
    buffers: Vec<Box<[u64]>>,
    // Each header needs a stable address while the Vec grows.
    #[allow(clippy::vec_box)]
    arrays: Vec<Box<AsterArray>>,
    error: Option<String>,
    collect_stats: bool,
    stats: MemoryStats,
}

impl ExecutionContext {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_stats() -> Self {
        Self {
            collect_stats: true,
            ..Self::default()
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

    fn record_allocation(
        &mut self,
        category: AllocationCategory,
        requested: usize,
        reserved: usize,
    ) {
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
        self.stats.used_bytes += reserved as u64;
        self.stats.reserved_bytes += reserved as u64;
        if self.stats.used_bytes > self.stats.peak_used_bytes {
            self.stats.peak_used_bytes = self.stats.used_bytes;
        }
        if self.stats.reserved_bytes > self.stats.peak_reserved_bytes {
            self.stats.peak_reserved_bytes = self.stats.reserved_bytes;
        }
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
        let words = bytes.div_ceil(size_of::<u64>());
        let reserved = words * size_of::<u64>();
        let mut buffer = vec![0_u64; words].into_boxed_slice();
        let data = buffer.as_mut_ptr().cast::<u8>();
        self.buffers.push(buffer);
        let mut header = Box::new(AsterArray {
            data,
            length: valid_length,
            element_size: valid_size,
        });
        let pointer = ptr::from_mut(header.as_mut());
        self.arrays.push(header);
        self.record_allocation(
            AllocationCategory::Array,
            bytes,
            reserved + size_of::<AsterArray>(),
        );
        pointer
    }

    pub(crate) fn allocate_object(&mut self, size: u32) -> *mut u8 {
        let bytes = usize::try_from(size.max(1)).unwrap_or(1);
        let words = bytes.div_ceil(size_of::<u64>());
        let reserved = words * size_of::<u64>();
        let mut buffer = vec![0_u64; words].into_boxed_slice();
        let pointer = buffer.as_mut_ptr().cast::<u8>();
        self.buffers.push(buffer);
        self.record_allocation(AllocationCategory::Object, bytes, reserved);
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
        let words = total_bytes.div_ceil(size_of::<u64>()).max(1);
        let reserved = words * size_of::<u64>();
        let mut buffer = Vec::new();
        if buffer.try_reserve_exact(words).is_err() {
            self.fail("string allocation failed");
            return ptr::null();
        }
        buffer.resize(words, 0_u64);
        // SAFETY: `buffer` owns `words * size_of::<u64>()` writable bytes and
        // `total_bytes` is no larger than that rounded-up allocation.
        #[allow(unsafe_code)]
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), total_bytes)
        };
        bytes[..size_of::<usize>()].copy_from_slice(&payload_bytes.to_ne_bytes());
        let mut cursor = size_of::<usize>();
        for part in parts {
            let end = cursor + part.len();
            bytes[cursor..end].copy_from_slice(part.as_bytes());
            cursor = end;
        }
        let buffer = buffer.into_boxed_slice();
        let pointer = buffer.as_ptr().cast::<AsterStrHeader>();
        self.buffers.push(buffer);
        self.record_allocation(AllocationCategory::String, total_bytes, reserved);
        pointer
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
mod tests {
    use super::*;
    use crate::object::aster_rt_object_new;

    #[test]
    fn allocation_is_zeroed_and_bounds_errors_are_controlled() {
        let mut context = ExecutionContext::new();
        let context_pointer = &raw mut context;
        let array = aster_rt_array_new(context_pointer, 2, 4);
        assert_eq!(aster_rt_array_length(context_pointer, array), 2);
        assert!(!aster_rt_array_element(context_pointer, array, 0).is_null());
        assert_eq!(context.buffers[0][0], 0);
        assert!(!aster_rt_array_element(context_pointer, array, 2).is_null());
        assert!(context.take_error().unwrap().contains("outside"));
    }

    #[test]
    fn object_storage_is_zeroed_and_owned_by_the_context() {
        let mut context = ExecutionContext::new();
        let pointer = aster_rt_object_new(&raw mut context, 16);
        assert!(!pointer.is_null());
        assert_eq!(context.buffers.len(), 1);
        assert!(context.buffers[0].iter().all(|word| *word == 0));
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
}
