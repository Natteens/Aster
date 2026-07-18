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

/// Owns every dynamic allocation made by one JIT invocation.
/// Dropping the context releases all buffers and headers together.
#[derive(Default)]
pub struct ExecutionContext {
    buffers: Vec<Box<[u64]>>,
    // Each header needs a stable address while the Vec grows.
    #[allow(clippy::vec_box)]
    arrays: Vec<Box<AsterArray>>,
    error: Option<String>,
}

impl ExecutionContext {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    pub(crate) fn fail(&mut self, message: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(message.into());
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
        let mut buffer = vec![0_u64; bytes.div_ceil(size_of::<u64>())].into_boxed_slice();
        let data = buffer.as_mut_ptr().cast::<u8>();
        self.buffers.push(buffer);
        let mut header = Box::new(AsterArray {
            data,
            length: valid_length,
            element_size: valid_size,
        });
        let pointer = ptr::from_mut(header.as_mut());
        self.arrays.push(header);
        pointer
    }

    pub(crate) fn allocate_object(&mut self, size: u32) -> *mut u8 {
        let bytes = usize::try_from(size.max(1)).unwrap_or(1);
        let mut buffer = vec![0_u64; bytes.div_ceil(size_of::<u64>())].into_boxed_slice();
        let pointer = buffer.as_mut_ptr().cast::<u8>();
        self.buffers.push(buffer);
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
}
