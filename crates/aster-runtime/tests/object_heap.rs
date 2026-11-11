//! MEM-EXP-1 stages B and C: size-class heap and reference-count primitives.
#![cfg(feature = "mem-exp-heap")]
#![allow(unsafe_code)]
#![allow(
    clippy::cast_ptr_alignment,
    reason = "heap objects are 8-byte aligned by the allocator contract"
)]

use aster_runtime::ExecutionContext;
use aster_runtime::object_heap::{
    IMMORTAL, ObjectDescriptor, ObjectHeapStats, retain, set_count, strong_count,
};

/// A node: descriptor word, count word (H12), then one reference at 16.
static NODE_OFFSETS: [u32; 1] = [16];
static NODE: ObjectDescriptor = ObjectDescriptor {
    size: 24,
    reference_count: 1,
    reference_offsets: NODE_OFFSETS.as_ptr(),
};
/// A leaf with no references.
static LEAF: ObjectDescriptor = ObjectDescriptor {
    size: 16,
    reference_count: 0,
    reference_offsets: std::ptr::null(),
};

fn object(context: &mut ExecutionContext, descriptor: &'static ObjectDescriptor) -> *mut u8 {
    let object = context.allocate_heap_object(descriptor.size);
    assert!(!object.is_null());
    unsafe {
        object
            .cast::<*const ObjectDescriptor>()
            .write(std::ptr::from_ref(descriptor));
        set_count(object, 1);
    }
    object
}

fn link(parent: *mut u8, child: *mut u8) {
    unsafe { parent.add(16).cast::<*mut u8>().write(child) };
}

#[test]
fn a_released_block_is_reused_zeroed_and_classes_do_not_mix() {
    let mut context = ExecutionContext::new();
    let first = object(&mut context, &LEAF);
    assert_eq!(unsafe { context.release_object(first) }, 1);
    let other_class = object(&mut context, &NODE);
    assert_ne!(
        other_class, first,
        "a 24-byte object must not take a 16-byte block"
    );
    let reused = context.allocate_heap_object(16);
    assert_eq!(reused, first, "the released 16-byte block is reused");
    let bytes = unsafe { std::slice::from_raw_parts(reused, 16) };
    assert!(
        bytes.iter().all(|byte| *byte == 0),
        "reused storage is zeroed"
    );
    assert_eq!(
        context.object_heap_stats(),
        ObjectHeapStats {
            fresh_blocks: 2,
            reused_blocks: 1,
            released_blocks: 1,
        }
    );
}

#[test]
fn a_long_chain_is_released_without_native_recursion() {
    let mut context = ExecutionContext::new();
    let count = 200_000;
    let tail = object(&mut context, &LEAF);
    let mut head = tail;
    for _ in 0..count {
        let node = object(&mut context, &NODE);
        link(node, head);
        head = node;
    }
    assert_eq!(unsafe { context.release_object(head) }, count + 1);
}

#[test]
fn a_shared_child_lives_until_its_last_parent_is_released() {
    let mut context = ExecutionContext::new();
    let child = object(&mut context, &LEAF);
    let left = object(&mut context, &NODE);
    let right = object(&mut context, &NODE);
    link(left, child);
    link(right, child);
    unsafe { set_count(child, 2) };
    assert_eq!(unsafe { context.release_object(left) }, 1);
    assert_eq!(
        unsafe { strong_count(child) },
        1,
        "the child still has one owner"
    );
    assert_eq!(unsafe { context.release_object(right) }, 2);
}

#[test]
fn a_strong_cycle_is_never_released_and_never_freed_early() {
    let mut context = ExecutionContext::new();
    let a = object(&mut context, &NODE);
    let b = object(&mut context, &NODE);
    link(a, b);
    link(b, a);
    unsafe {
        set_count(a, 2); // the local and `b`
        set_count(b, 1); // `a`
    }
    assert_eq!(
        unsafe { context.release_object(a) },
        0,
        "the cycle keeps `a` alive"
    );
    assert_eq!(unsafe { strong_count(a) }, 1);
    assert_eq!(unsafe { strong_count(b) }, 1);
    assert_eq!(context.object_heap_stats().released_blocks, 0);
}

#[test]
fn immortal_objects_ignore_counting_and_counts_saturate() {
    let mut context = ExecutionContext::new();
    let immortal = object(&mut context, &LEAF);
    unsafe {
        set_count(immortal, IMMORTAL);
        retain(immortal);
        assert_eq!(context.release_object(immortal), 0);
        assert_eq!(strong_count(immortal), IMMORTAL);
    }
    let busy = object(&mut context, &LEAF);
    unsafe {
        set_count(busy, IMMORTAL - 1);
        retain(busy);
        assert_eq!(
            strong_count(busy),
            IMMORTAL,
            "saturation leaks, never frees early"
        );
        assert_eq!(context.release_object(busy), 0);
    }
}

#[test]
fn objects_larger_than_every_size_class_bypass_the_heap() {
    let mut context = ExecutionContext::new();
    let large = context.allocate_heap_object(4096);
    assert!(!large.is_null());
    assert_eq!(context.object_heap_stats().fresh_blocks, 0);
}
