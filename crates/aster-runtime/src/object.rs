//! Object allocation at the JIT/runtime boundary.

use std::ptr;

use crate::ExecutionContext;

/// Allocate stable, zeroed storage owned by one execution context.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_object_new(context: *mut ExecutionContext, size: i32) -> *mut u8 {
    if context.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: generated code receives this live host-owned pointer as its
    // hidden first argument and cannot retain it beyond the invocation.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.allocate_object(u32::try_from(size).unwrap_or(1))
}
