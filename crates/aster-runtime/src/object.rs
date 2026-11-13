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

/// Allocate zeroed object storage in the innermost active temporary scope.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_object_new_temporary(
    context: *mut ExecutionContext,
    size: i32,
) -> *mut u8 {
    if context.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: generated code receives this live host-owned pointer as its
    // hidden first argument and cannot retain it beyond the invocation.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    context.allocate_temporary_object(u32::try_from(size).unwrap_or(1))
}

/// Add one strong reference to a counted object (MEM-EXP-1 stage C). Null and
/// immortal objects are ignored.
#[cfg(feature = "mem-exp-rc")]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_object_retain(context: *mut ExecutionContext, object: *mut u8) {
    if context.is_null() || object.is_null() {
        return;
    }
    // SAFETY: generated code passes its live host-owned context, and a
    // non-null object word always points to an H12 object of that context.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    #[allow(unsafe_code)]
    unsafe {
        context.retain_object(object);
    }
}

/// Drop one strong reference (MEM-EXP-1 stage C), releasing the object and its
/// reference fields when the count reaches zero. Null and immortal objects are
/// ignored.
#[cfg(feature = "mem-exp-rc")]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_object_release(context: *mut ExecutionContext, object: *mut u8) {
    if context.is_null() || object.is_null() {
        return;
    }
    // SAFETY: as in `aster_rt_object_retain`.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    #[allow(unsafe_code)]
    unsafe {
        context.release_counted_object(object);
    }
}

/// Slow path of inline generated Release for an object whose count is one
/// (MEM-EXP-1 section 13). The generated code already incremented the executed
/// `releases` counter, so this does not count again.
#[cfg(feature = "mem-exp-rc")]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_object_release_last(context: *mut ExecutionContext, object: *mut u8) {
    if context.is_null() || object.is_null() {
        return;
    }
    // SAFETY: as in `aster_rt_object_retain`.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    #[allow(unsafe_code)]
    unsafe {
        context.release_last_object(object);
    }
}
