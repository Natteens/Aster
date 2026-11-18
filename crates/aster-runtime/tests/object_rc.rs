//! MEM-EXP-1 stage C: count initialization and the exported retain/release.
#![cfg(feature = "mem-exp-rc")]
#![allow(unsafe_code, clippy::cast_ptr_alignment)]

use aster_runtime::object_heap::{IMMORTAL, ObjectDescriptor, strong_count};
use aster_runtime::{ExecutionContext, runtime_functions};

static LEAF: ObjectDescriptor = ObjectDescriptor {
    size: 16,
    reference_count: 0,
    reference_offsets: std::ptr::null(),
};

type Retain = extern "C" fn(*mut ExecutionContext, *mut u8);

fn function(name: &str) -> Retain {
    let found = runtime_functions()
        .into_iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing runtime function {name}"));
    // SAFETY: both exports have the signature (pointer, pointer) -> ().
    unsafe { std::mem::transmute::<*const u8, Retain>(found.address) }
}

#[test]
fn persistent_allocation_starts_at_one_and_oversized_is_immortal() {
    let mut context = ExecutionContext::new();
    let small = context.allocate_heap_object(24);
    assert_eq!(unsafe { strong_count(small) }, 1);
    let large = context.allocate_heap_object(4096);
    assert_eq!(unsafe { strong_count(large) }, IMMORTAL);
}

#[test]
fn exported_retain_and_release_count_and_ignore_null() {
    let mut context = ExecutionContext::with_stats();
    let context_pointer: *mut ExecutionContext = &raw mut context;
    let retain = function("aster_rt_object_retain");
    let release = function("aster_rt_object_release");
    retain(context_pointer, std::ptr::null_mut());
    release(context_pointer, std::ptr::null_mut());

    let object = context.allocate_heap_object(16);
    unsafe {
        object
            .cast::<*const ObjectDescriptor>()
            .write(std::ptr::from_ref(&LEAF));
    }
    retain(context_pointer, object);
    assert_eq!(unsafe { strong_count(object) }, 2);
    release(context_pointer, object);
    assert_eq!(unsafe { strong_count(object) }, 1);
    release(context_pointer, object);
    let stats = context.object_heap_stats();
    assert_eq!(
        (stats.retains, stats.releases, stats.released_blocks),
        (1, 2, 1)
    );
    assert_eq!(context.memory_stats().object_heap, stats);
}

#[test]
fn immortal_objects_are_not_counted() {
    let mut context = ExecutionContext::new();
    let context_pointer: *mut ExecutionContext = &raw mut context;
    let large = context.allocate_heap_object(4096);
    function("aster_rt_object_retain")(context_pointer, large);
    function("aster_rt_object_release")(context_pointer, large);
    let stats = context.object_heap_stats();
    assert_eq!((stats.retains, stats.releases), (0, 0));
}
