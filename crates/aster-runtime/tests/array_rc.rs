//! MEM-EXP-1 stage C, U8: counted arrays (uniform header, ARRAY flag, release
//! work list, element ownership).
#![cfg(feature = "mem-exp-rc")]
#![allow(
    unsafe_code,
    clippy::cast_ptr_alignment,
    clippy::many_single_char_names
)]

use aster_runtime::object_heap::{ARRAY_FLAG, IMMORTAL, ObjectDescriptor, strong_count};
use aster_runtime::{
    ASTER_ARRAY_COUNT_OFFSET, ASTER_ARRAY_DATA_OFFSET, ASTER_ARRAY_LENGTH_OFFSET, AsterArray,
    ExecutionContext, aster_rt_array_adopt_elements, aster_rt_array_element, aster_rt_array_new,
    aster_rt_array_new_temporary, aster_rt_array_set_element_descriptor,
    aster_rt_temporary_scope_enter,
};

static LEAF: ObjectDescriptor = ObjectDescriptor {
    size: 16,
    reference_count: 0,
    reference_offsets: std::ptr::null(),
};

// One element = one leaf at offset 0 (a class reference or an array).
static OFFSETS: [u32; 1] = [0];
static ELEMENT: ObjectDescriptor = ObjectDescriptor {
    size: 8,
    reference_count: 1,
    reference_offsets: OFFSETS.as_ptr(),
};

struct Fixture {
    context: Box<ExecutionContext>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            context: Box::new(ExecutionContext::with_stats()),
        }
    }

    fn cx(&mut self) -> *mut ExecutionContext {
        &raw mut *self.context
    }

    fn object(&mut self) -> *mut u8 {
        let object = self.context.allocate_heap_object(16);
        unsafe {
            object
                .cast::<*const ObjectDescriptor>()
                .write(&raw const LEAF);
        }
        object
    }

    fn array(&mut self, length: i32, element_size: i32) -> *mut AsterArray {
        let cx = self.cx();
        aster_rt_array_new(cx, length, element_size)
    }

    fn store(&mut self, array: *mut AsterArray, index: i32, value: *mut u8) {
        let cx = self.cx();
        let slot = aster_rt_array_element(cx, array, index);
        unsafe { slot.cast::<*mut u8>().write(value) };
    }

    fn release(&mut self, object: *mut u8) {
        unsafe { self.context.release_counted_object(object) };
    }
}

fn count(object: *mut u8) -> u32 {
    unsafe { strong_count(object) }
}

#[test]
fn header_layout_is_uniform() {
    assert_eq!(ASTER_ARRAY_COUNT_OFFSET, 8);
    assert_eq!(ASTER_ARRAY_LENGTH_OFFSET, 12);
    assert_eq!(ASTER_ARRAY_DATA_OFFSET, 24);
    assert_eq!(size_of::<AsterArray>(), 32);
}

#[test]
fn persistent_array_starts_at_one_with_the_array_flag() {
    let mut fx = Fixture::new();
    let array = fx.array(4, 4);
    assert_eq!(count(array.cast()), 1);
    let word = unsafe {
        array
            .cast::<u8>()
            .add(ASTER_ARRAY_COUNT_OFFSET)
            .cast::<u32>()
            .read()
    };
    assert_eq!(word, 1 | ARRAY_FLAG);
    // Flags survive retain and strong_count masks them.
    unsafe { fx.context.retain_object(array.cast()) };
    assert_eq!(count(array.cast()), 2);
}

#[test]
fn release_frees_header_and_data_and_both_are_reused() {
    let mut fx = Fixture::new();
    let first = fx.array(8, 4);
    assert_eq!(
        fx.context.object_heap_stats().fresh_blocks,
        2,
        "header block and data block"
    );
    fx.release(first.cast());
    let stats = fx.context.object_heap_stats();
    assert_eq!(stats.released_blocks, 2);
    assert_eq!(stats.released_arrays, 1);
    let second = fx.array(8, 4);
    let stats = fx.context.object_heap_stats();
    assert_eq!(stats.reused_blocks, 2, "same size classes are reused");
    assert_eq!(stats.fresh_blocks, 2);
    // Reused storage is zeroed and counted again.
    assert_eq!(count(second.cast()), 1);
    let slot = aster_rt_array_element(fx.cx(), second, 7);
    assert_eq!(unsafe { slot.cast::<u32>().read() }, 0);
}

#[test]
fn element_leaves_are_released_when_the_array_dies() {
    let mut fx = Fixture::new();
    let array = fx.array(3, 8);
    let cx = fx.cx();
    aster_rt_array_set_element_descriptor(cx, array, &raw const ELEMENT);
    let objects: Vec<_> = (0..3).map(|_| fx.object()).collect();
    for (index, object) in objects.iter().enumerate() {
        fx.store(array, i32::try_from(index).unwrap(), *object);
    }
    // A second holder on the middle object survives the array.
    unsafe { fx.context.retain_object(objects[1]) };
    fx.release(array.cast());
    let stats = fx.context.object_heap_stats();
    // Two objects, the array header and its data block.
    assert_eq!(stats.released_blocks, 4);
    assert_eq!(count(objects[1]), 1);
    fx.release(objects[1]);
    assert_eq!(fx.context.object_heap_stats().released_blocks, 5);
}

#[test]
fn adopt_retains_every_leaf() {
    let mut fx = Fixture::new();
    let array = fx.array(2, 8);
    let a = fx.object();
    let b = fx.object();
    fx.store(array, 0, a);
    fx.store(array, 1, b);
    let cx = fx.cx();
    aster_rt_array_adopt_elements(cx, array, &raw const ELEMENT);
    assert_eq!((count(a), count(b)), (2, 2));
    fx.release(array.cast());
    assert_eq!((count(a), count(b)), (1, 1));
}

#[test]
fn array_of_arrays_is_released_without_recursion() {
    let mut fx = Fixture::new();
    let depth = 100_000_u64;
    let mut inner = fx.array(1, 8);
    let cx = fx.cx();
    aster_rt_array_set_element_descriptor(cx, inner, &raw const ELEMENT);
    let leaf = fx.object();
    fx.store(inner, 0, leaf);
    for _ in 0..depth {
        let outer = fx.array(1, 8);
        aster_rt_array_set_element_descriptor(cx, outer, &raw const ELEMENT);
        fx.store(outer, 0, inner.cast());
        inner = outer;
    }
    fx.release(inner.cast());
    let stats = fx.context.object_heap_stats();
    assert_eq!(stats.released_arrays, depth + 1);
    // Every array frees two blocks, plus the leaf object.
    assert_eq!(stats.released_blocks, (depth + 1) * 2 + 1);
}

#[test]
fn temporary_arrays_are_immortal() {
    let mut fx = Fixture::new();
    let cx = fx.cx();
    aster_rt_temporary_scope_enter(cx);
    let array = aster_rt_array_new_temporary(cx, 4, 4);
    assert_eq!(count(array.cast()), IMMORTAL);
    unsafe { fx.context.retain_object(array.cast()) };
    fx.release(array.cast());
    assert_eq!(count(array.cast()), IMMORTAL);
    assert_eq!(fx.context.object_heap_stats().released_blocks, 0);
}

#[test]
fn data_above_the_largest_size_class_stays_reserved() {
    let mut fx = Fixture::new();
    let array = fx.array(262_145, 4); // 1 MiB + 4 bytes of data
    assert_eq!(
        fx.context.object_heap_stats().fresh_blocks,
        1,
        "header only"
    );
    fx.release(array.cast());
    let stats = fx.context.object_heap_stats();
    assert_eq!(stats.released_blocks, 1, "only the header returns");
    assert_eq!(stats.released_arrays, 1);
    assert_eq!(stats.unreturned_array_blocks, 1);
    // A 2,048-byte payload is heap-served from a small class.
    let small = fx.array(512, 4);
    fx.release(small.cast());
    let stats = fx.context.object_heap_stats();
    assert_eq!(stats.unreturned_array_blocks, 1);
    assert_eq!(stats.released_blocks, 3);
}

#[test]
fn zero_length_arrays_are_counted_and_freed() {
    let mut fx = Fixture::new();
    let array = fx.array(0, 4);
    assert_eq!(count(array.cast()), 1);
    fx.release(array.cast());
    assert_eq!(fx.context.object_heap_stats().released_blocks, 2);
}

#[test]
fn large_array_data_is_returned_and_reused() {
    let mut fx = Fixture::new();
    let first = fx.array(2048, 4); // 8 KiB data: a large class
    let fresh = fx.context.object_heap_stats().fresh_blocks;
    assert_eq!(fresh, 2, "header and data block");
    fx.release(first.cast());
    let stats = fx.context.object_heap_stats();
    assert_eq!(stats.released_blocks, 2);
    assert_eq!(stats.unreturned_array_blocks, 0);
    // The same size again reuses both blocks and sees zeroed data.
    let second = fx.array(2000, 4);
    let stats = fx.context.object_heap_stats();
    assert_eq!(stats.fresh_blocks, fresh);
    assert_eq!(stats.reused_blocks, 2);
    let data = unsafe {
        second
            .cast::<u8>()
            .add(ASTER_ARRAY_DATA_OFFSET)
            .cast::<*const u8>()
            .read()
    };
    let bytes = unsafe { std::slice::from_raw_parts(data, 8000) };
    assert!(bytes.iter().all(|&byte| byte == 0));
}
