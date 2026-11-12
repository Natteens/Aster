//! JIT-backed tests moved from `aster-exec::task_abi` (ARCH-005).

use aster_exec::task_abi::*;
use aster_exec::task_runtime::*;
use aster_runtime::ExecutionContext;

use crate::provider::provider;
use std::sync::Arc;

use aster_runtime::MemoryGovernor;

#[test]
fn waiting_on_an_invalid_handle_is_a_controlled_error_not_a_dereference() {
    let module = aster_compiler::compile("public int Compute() { return 1; }")
        .expect("source compiles")
        .mir;
    let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), 1)
        .expect("runtime starts with no tasks yet");
    let mut context = ExecutionContext::new();
    context.set_task_runtime(std::ptr::from_mut(&mut runtime).cast::<()>());

    let bogus_handle: i64 = i64::MAX;
    let result = aster_task_wait_i32(&raw mut context, bogus_handle);

    assert_eq!(result, 0);
    assert!(
        context
            .take_error()
            .is_some_and(|error| error.contains("unknown task handle"))
    );
}

#[test]
fn waiting_through_an_incompatible_abi_type_reports_a_controlled_error() {
    let module = aster_compiler::compile("public long Compute() { return 1L; }")
        .expect("source compiles")
        .mir;
    let symbol = module.functions[0].symbol;
    let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), 1)
        .expect("runtime starts with no tasks yet");
    let handle = runtime.run(symbol).expect("task is accepted");
    let mut context = ExecutionContext::new();
    context.set_task_runtime(std::ptr::from_mut(&mut runtime).cast::<()>());

    let result = aster_task_wait_i32(&raw mut context, handle.to_bits());

    assert_eq!(result, 0);
    assert!(
        context
            .take_error()
            .is_some_and(|error| error.contains("expected an i32-compatible"))
    );
}

#[test]
#[cfg_attr(
    feature = "mem-exp-rc-core",
    ignore = "MEM-EXP-1 counted arrays use a separate header and a 4 KiB data block; the one-page budget is calibrated for the single-block layout"
)]
fn wait_all_allocation_denial_exposes_no_partial_result_and_keeps_handles_valid() {
    let module = aster_compiler::compile("public int Compute() { return 1; }")
        .expect("source compiles")
        .mir;
    let symbol = module.functions[0].symbol;
    let mut runtime = TaskRuntime::new(&provider(), &Arc::new(module), 4)
        .expect("runtime starts with no tasks yet");
    let handles = (0..500)
        .map(|_| runtime.run(symbol).expect("task is accepted"))
        .collect::<Vec<_>>();
    let governor = Arc::new(MemoryGovernor::new(
        ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    ));
    let mut context = ExecutionContext::with_memory_governor(Arc::clone(&governor));
    context.set_task_runtime(std::ptr::from_mut(&mut runtime).cast::<()>());
    let tasks = aster_runtime::context::aster_rt_array_new(&raw mut context, 500, 8);
    assert!(!tasks.is_null(), "the input handle array fits one page");
    for (index, handle) in handles.iter().enumerate() {
        let element = aster_runtime::aster_rt_array_element(
            &raw mut context,
            tasks,
            i32::try_from(index).expect("small task index"),
        );
        // SAFETY: the just-created Task<int>[] has initialized eight-byte
        // handle slots owned exclusively by this test context.
        #[allow(unsafe_code)]
        unsafe {
            element.cast::<i64>().write_unaligned(handle.to_bits());
        }
    }

    let result = aster_task_wait_all_i32(&raw mut context, tasks);
    assert!(result.is_null());
    let error = context
        .take_error()
        .expect("allocation denial is controlled");
    assert!(error.contains("execution memory budget"), "{error}");

    assert_eq!(
        aster_task_wait_i32(&raw mut context, handles[0].to_bits()),
        1,
        "WaitAll caches terminal outcomes before caller-array allocation"
    );
    assert_eq!(
        aster_task_wait_i32(&raw mut context, handles[499].to_bits()),
        1,
        "WaitAll reaches the final input before attempting result allocation"
    );
    assert!(context.take_error().is_none());
    drop(context);
    assert_eq!(governor.telemetry().current_capacity_bytes, 0);
}

#[test]
fn a_null_context_never_dereferences_anything() {
    assert_eq!(aster_task_run(std::ptr::null_mut(), 0), 0);
    assert_eq!(aster_task_wait_i32(std::ptr::null_mut(), 0), 0);
}
