//! Narrow runtime boundary for math-domain failures.

use crate::ExecutionContext;

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_math_domain_error(context: *mut ExecutionContext, code: i32) {
    if context.is_null() {
        return;
    }
    // SAFETY: JIT-generated calls receive the live host-owned context as their
    // hidden first argument and cannot retain it after the invocation.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let message = match code {
        0 => "Math.Abs cannot represent the magnitude of the minimum int value",
        1 => "Math.Abs cannot represent the magnitude of the minimum long value",
        2 => "Math.Clamp requires min to be less than or equal to max",
        _ => "unknown runtime error",
    };
    context.fail(message);
}

#[cfg(test)]
mod tests {
    use super::aster_rt_math_domain_error;
    use crate::ExecutionContext;

    #[test]
    fn records_a_controlled_domain_error() {
        let mut context = ExecutionContext::new();
        aster_rt_math_domain_error(&raw mut context, 2);
        assert!(context.take_error().unwrap().contains("min"));
    }
}
