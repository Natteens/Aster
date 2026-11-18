//! MEM-EXP-1 stage C, U8: runtime glue for `List<T>` element ownership.
#![cfg(feature = "mem-exp-rc")]
#![allow(
    unsafe_code,
    clippy::cast_ptr_alignment,
    clippy::many_single_char_names
)]

use aster_runtime::object_heap::{
    IMMORTAL, KIND_LIST, KIND_MASK, KIND_SHIFT, LIST_FLAG, ObjectDescriptor, strong_count,
};
use aster_runtime::{
    ASTER_ARRAY_DATA_OFFSET, ASTER_LIST_COUNT_OFFSET, ASTER_LIST_DESCRIPTOR_OFFSET, AsterArray,
    AsterList, ExecutionContext, aster_rt_list_add, aster_rt_list_add_range, aster_rt_list_clear,
    aster_rt_list_get_range, aster_rt_list_new, aster_rt_list_new_temporary,
    aster_rt_list_remove_at, aster_rt_list_remove_range, aster_rt_list_set,
    aster_rt_list_set_element_descriptor, aster_rt_list_to_array, aster_rt_temporary_scope_enter,
};

static LEAF: ObjectDescriptor = ObjectDescriptor {
    size: 16,
    reference_count: 0,
    reference_offsets: std::ptr::null(),
};

// Element = two leaves at offsets 0 and 8 (an inline struct of two classes).
static OFFSETS: [u32; 2] = [0, 8];
static ELEMENT: ObjectDescriptor = ObjectDescriptor {
    size: 16,
    reference_count: 2,
    reference_offsets: OFFSETS.as_ptr(),
};

const KEY: i64 = 77;

struct Fixture {
    context: Box<ExecutionContext>,
    list: *mut AsterList,
}

impl Fixture {
    fn new() -> Self {
        let mut context = Box::new(ExecutionContext::with_stats());
        let pointer: *mut ExecutionContext = &raw mut *context;
        let list = aster_rt_list_new(pointer, 16, 8, KEY);
        aster_rt_list_set_element_descriptor(pointer, list, &raw const ELEMENT);
        Self { context, list }
    }

    fn cx(&mut self) -> *mut ExecutionContext {
        &raw mut *self.context
    }

    /// A counted leaf object whose only count is the one handed to the list.
    fn object(&mut self) -> *mut u8 {
        let object = self.context.allocate_heap_object(16);
        unsafe {
            object
                .cast::<*const ObjectDescriptor>()
                .write(&raw const LEAF);
        }
        object
    }

    fn add(&mut self, a: *mut u8, b: *mut u8) {
        let element = [a, b];
        let cx = self.cx();
        assert_eq!(
            aster_rt_list_add(cx, self.list, 16, 8, KEY, element.as_ptr().cast()),
            1
        );
    }

    fn set(&mut self, index: i32, a: *mut u8, b: *mut u8) {
        let element = [a, b];
        let cx = self.cx();
        assert_eq!(
            aster_rt_list_set(cx, self.list, 16, 8, KEY, index, element.as_ptr().cast()),
            1
        );
    }
}

fn count(object: *mut u8) -> u32 {
    unsafe { strong_count(object) }
}

fn array_data(array: *mut AsterArray) -> *mut *mut u8 {
    unsafe {
        array
            .cast::<u8>()
            .add(ASTER_ARRAY_DATA_OFFSET)
            .cast::<*mut u8>()
            .read()
            .cast::<*mut u8>()
    }
}

#[test]
fn set_releases_old_leaves_after_store() {
    let mut f = Fixture::new();
    let (a, b, c, d) = (f.object(), f.object(), f.object(), f.object());
    f.add(a, b);
    f.set(0, c, d);
    // The old element's two counts were released and the blocks reused.
    assert_eq!(f.context.object_heap_stats().released_blocks, 2);
    assert_eq!((count(c), count(d)), (1, 1));
    let again = f.object();
    assert!(again == a || again == b);
}

#[test]
fn set_with_the_same_object_keeps_it_alive() {
    let mut f = Fixture::new();
    let (a, b) = (f.object(), f.object());
    f.add(a, b);
    // The pass retains the stored value first: count 2, then release the old.
    unsafe {
        let cx = f.cx();
        (*cx).retain_object(a);
        (*cx).retain_object(b);
    }
    f.set(0, a, b);
    assert_eq!((count(a), count(b)), (1, 1));
    assert_eq!(f.context.object_heap_stats().released_blocks, 0);
}

#[test]
fn remove_at_releases_removed_after_shift() {
    let mut f = Fixture::new();
    let (a, b, c, d) = (f.object(), f.object(), f.object(), f.object());
    f.add(a, b);
    f.add(c, d);
    let cx = f.cx();
    assert_eq!(aster_rt_list_remove_at(cx, f.list, 16, 8, KEY, 0), 1);
    assert_eq!(f.context.object_heap_stats().released_blocks, 2);
    assert_eq!((count(c), count(d)), (1, 1));
}

#[test]
fn remove_range_releases_every_removed_leaf() {
    let mut f = Fixture::new();
    let objects: Vec<*mut u8> = (0..6).map(|_| f.object()).collect();
    for pair in objects.chunks(2) {
        f.add(pair[0], pair[1]);
    }
    let cx = f.cx();
    assert_eq!(aster_rt_list_remove_range(cx, f.list, 1, 2), 1);
    assert_eq!(f.context.object_heap_stats().released_blocks, 4);
    // Only the first element survives; nothing of it was released.
    assert_eq!((count(objects[0]), count(objects[1])), (1, 1));
}

#[test]
fn clear_releases_all_former_elements() {
    let mut f = Fixture::new();
    let objects: Vec<*mut u8> = (0..6).map(|_| f.object()).collect();
    for pair in objects.chunks(2) {
        f.add(pair[0], pair[1]);
    }
    let cx = f.cx();
    assert_eq!(aster_rt_list_clear(cx, f.list, 16, 8, KEY), 1);
    assert_eq!(f.context.object_heap_stats().released_blocks, 6);
}

#[test]
fn to_array_get_range_and_add_range_retain_copies() {
    let mut f = Fixture::new();
    let (a, b, c, d) = (f.object(), f.object(), f.object(), f.object());
    f.add(a, b);
    f.add(c, d);
    let cx = f.cx();

    let array = aster_rt_list_to_array(cx, f.list, 16, 8, KEY, 0);
    assert!(!array.is_null());
    assert_eq!([a, b, c, d].map(count), [2, 2, 2, 2]);
    assert_eq!(unsafe { array_data(array).read() }, a);

    let range = aster_rt_list_get_range(cx, f.list, 1, 1, 0);
    assert!(!range.is_null());
    assert_eq!([a, b, c, d].map(count), [2, 2, 3, 3]);
    assert_eq!(unsafe { array_data(range).read() }, c);

    // The array copy keeps the old object readable after the list replaces it.
    let (e, g) = (f.object(), f.object());
    f.set(0, e, g);
    assert_eq!([a, b].map(count), [1, 1]);
    assert_eq!(unsafe { array_data(array).read() }, a);

    let cx = f.cx();
    assert_eq!(aster_rt_list_add_range(cx, f.list, array), 1);
    assert_eq!([a, b, c, d].map(count), [2, 2, 4, 4]);
}

#[test]
fn immortal_and_null_leaves_are_ignored() {
    let mut f = Fixture::new();
    let big = f.context.allocate_heap_object(4096);
    assert_eq!(count(big), IMMORTAL);
    f.add(big, std::ptr::null_mut());
    f.add(std::ptr::null_mut(), big);
    let cx = f.cx();
    let array = aster_rt_list_to_array(cx, f.list, 16, 8, KEY, 0);
    assert!(!array.is_null());
    f.set(0, std::ptr::null_mut(), std::ptr::null_mut());
    let cx = f.cx();
    assert_eq!(aster_rt_list_remove_at(cx, f.list, 16, 8, KEY, 0), 1);
    assert_eq!(aster_rt_list_clear(cx, f.list, 16, 8, KEY), 1);
    let stats = f.context.object_heap_stats();
    assert_eq!((stats.retains, stats.releases), (0, 0));
    assert_eq!(count(big), IMMORTAL);
}

#[test]
fn list_without_descriptor_does_no_counting() {
    let mut context = Box::new(ExecutionContext::with_stats());
    let cx: *mut ExecutionContext = &raw mut *context;
    let list = aster_rt_list_new(cx, 8, 8, KEY);
    let object = context.allocate_heap_object(16);
    let element = [object];
    assert_eq!(
        aster_rt_list_add(cx, list, 8, 8, KEY, element.as_ptr().cast()),
        1
    );
    assert_eq!(aster_rt_list_clear(cx, list, 8, 8, KEY), 1);
    assert_eq!(count(object), 1);
}

// ---- Counted list headers (design section 10) ----

fn count_word(object: *mut u8) -> u32 {
    unsafe { object.add(ASTER_LIST_COUNT_OFFSET).cast::<u32>().read() }
}

fn release(context: &mut ExecutionContext, object: *mut u8) {
    unsafe { context.release_counted_object(object) };
}

// One element = one leaf at offset 0 (a class object or another list).
static ONE_OFFSET: [u32; 1] = [0];
static ONE_LEAF: ObjectDescriptor = ObjectDescriptor {
    size: 8,
    reference_count: 1,
    reference_offsets: ONE_OFFSET.as_ptr(),
};

fn leaf_list(context: &mut ExecutionContext) -> *mut AsterList {
    let cx: *mut ExecutionContext = &raw mut *context;
    let list = aster_rt_list_new(cx, 8, 8, KEY);
    aster_rt_list_set_element_descriptor(cx, list, &raw const ONE_LEAF);
    list
}

fn push(context: &mut ExecutionContext, list: *mut AsterList, value: *mut u8) {
    let cx: *mut ExecutionContext = &raw mut *context;
    let element = [value];
    assert_eq!(
        aster_rt_list_add(cx, list, 8, 8, KEY, element.as_ptr().cast()),
        1
    );
}

#[test]
fn list_header_has_the_uniform_prefix_and_kind() {
    assert_eq!(ASTER_LIST_DESCRIPTOR_OFFSET, 0);
    assert_eq!(ASTER_LIST_COUNT_OFFSET, 8);
    assert_eq!(KIND_LIST, 3);
    assert_eq!(KIND_SHIFT, 29);
    assert_eq!(LIST_FLAG, 3 << 29);
    assert_eq!(KIND_MASK, 0b111 << 29);
    let mut context = Box::new(ExecutionContext::with_stats());
    let list = leaf_list(&mut context);
    let descriptor = unsafe { list.cast::<*const ObjectDescriptor>().read() };
    assert_eq!(descriptor, &raw const ONE_LEAF);
}

#[test]
fn persistent_list_starts_at_one_with_the_list_kind() {
    let mut context = Box::new(ExecutionContext::with_stats());
    let list = leaf_list(&mut context);
    assert_eq!(count_word(list.cast()), 1 | LIST_FLAG);
    assert_eq!(count(list.cast()), 1);
    unsafe { context.retain_object(list.cast()) };
    assert_eq!(count(list.cast()), 2);
    assert_eq!(count_word(list.cast()) & KIND_MASK, LIST_FLAG);
    // The header is served by a size class.
    assert_eq!(context.object_heap_stats().fresh_blocks, 1);
}

#[test]
fn release_frees_elements_buffer_and_header_and_blocks_are_reused() {
    let mut context = Box::new(ExecutionContext::with_stats());
    let list = leaf_list(&mut context);
    let objects: Vec<*mut u8> = (0..3)
        .map(|_| {
            let object = context.allocate_heap_object(16);
            unsafe {
                object
                    .cast::<*const ObjectDescriptor>()
                    .write(&raw const LEAF);
            }
            object
        })
        .collect();
    for object in &objects {
        push(&mut context, list, *object);
    }
    // header + buffer (4 elements x 8 bytes) + 3 objects.
    assert_eq!(context.object_heap_stats().fresh_blocks, 5);
    // A second holder of the middle object survives the list.
    unsafe { context.retain_object(objects[1]) };
    release(&mut context, list.cast());
    let stats = context.object_heap_stats();
    assert_eq!(stats.released_lists, 1);
    // header + buffer + two objects.
    assert_eq!(stats.released_blocks, 4);
    assert_eq!(count(objects[1]), 1);
    release(&mut context, objects[1]);
    assert_eq!(context.object_heap_stats().released_blocks, 5);

    // The same classes are served again without fresh arena blocks.
    let second = leaf_list(&mut context);
    push(&mut context, second, std::ptr::null_mut());
    let stats = context.object_heap_stats();
    assert_eq!(stats.fresh_blocks, 5);
    assert_eq!(stats.reused_blocks, 2);
    assert_eq!(count(second.cast()), 1);
}

#[test]
fn growth_returns_the_old_buffer_to_the_heap() {
    let mut context = Box::new(ExecutionContext::with_stats());
    let list = leaf_list(&mut context);
    for _ in 0..4 {
        push(&mut context, list, std::ptr::null_mut());
    }
    let before = context.object_heap_stats();
    assert_eq!(before.released_blocks, 0);
    // The fifth element doubles 32 bytes to 64: the old buffer returns.
    push(&mut context, list, std::ptr::null_mut());
    let after = context.object_heap_stats();
    assert_eq!(after.released_blocks, 1);
    assert_eq!(after.fresh_blocks, before.fresh_blocks + 1);
    // A new list's 32-byte buffer reuses the returned block.
    let other = leaf_list(&mut context);
    push(&mut context, other, std::ptr::null_mut());
    assert_eq!(context.object_heap_stats().reused_blocks, 1);
    release(&mut context, list.cast());
    assert_eq!(context.object_heap_stats().released_lists, 1);
}

#[test]
fn buffers_above_the_largest_size_class_stay_reserved() {
    let mut context = Box::new(ExecutionContext::with_stats());
    let list = leaf_list(&mut context);
    // 131,073 elements x 8 bytes: the buffer grows past the 1 MiB class.
    for _ in 0..131_073 {
        push(&mut context, list, std::ptr::null_mut());
    }
    let grown = context.object_heap_stats();
    release(&mut context, list.cast());
    let stats = context.object_heap_stats();
    assert_eq!(stats.unreturned_list_buffers, 1);
    assert_eq!(stats.released_lists, 1);
    // Only the header returns for the final buffer.
    assert_eq!(stats.released_blocks, grown.released_blocks + 1);
}

#[test]
fn list_of_lists_is_released_without_recursion() {
    let mut context = Box::new(ExecutionContext::with_stats());
    let depth = 100_000_u64;
    let mut inner = leaf_list(&mut context);
    for _ in 0..depth {
        let outer = leaf_list(&mut context);
        push(&mut context, outer, inner.cast());
        inner = outer;
    }
    release(&mut context, inner.cast());
    let stats = context.object_heap_stats();
    assert_eq!(stats.released_lists, depth + 1);
    // Every list but the innermost frees a header and a buffer.
    assert_eq!(stats.released_blocks, depth * 2 + 1);
}

#[test]
fn temporary_lists_are_immortal() {
    let mut context = Box::new(ExecutionContext::with_stats());
    let cx: *mut ExecutionContext = &raw mut *context;
    aster_rt_temporary_scope_enter(cx);
    let list = aster_rt_list_new_temporary(cx, 8, 8, KEY);
    assert_eq!(count(list.cast()), IMMORTAL);
    assert_eq!(count_word(list.cast()), IMMORTAL | LIST_FLAG);
    unsafe { context.retain_object(list.cast()) };
    release(&mut context, list.cast());
    assert_eq!(count(list.cast()), IMMORTAL);
    push(&mut context, list, std::ptr::null_mut());
    assert_eq!(context.object_heap_stats().released_blocks, 0);
}

#[test]
fn growth_across_the_small_classes_returns_and_reuses_large_buffers() {
    let mut context = Box::new(ExecutionContext::with_stats());
    let list = leaf_list(&mut context);
    // 1,000 x 8 bytes: capacity 1,024 = 8 KiB, grown through 4 KiB.
    for _ in 0..1000 {
        push(&mut context, list, std::ptr::null_mut());
    }
    let grown = context.object_heap_stats();
    // Every replaced buffer went back (4..2048 bytes through the 4 KiB one).
    assert_eq!(grown.released_blocks, 8);
    assert_eq!(grown.unreturned_list_buffers, 0);
    release(&mut context, list.cast());
    let stats = context.object_heap_stats();
    assert_eq!(stats.unreturned_list_buffers, 0);
    assert_eq!(stats.released_blocks, grown.released_blocks + 2);
    // A second list of the same size is served entirely by reused blocks.
    let fresh = stats.fresh_blocks;
    let second = leaf_list(&mut context);
    for _ in 0..1000 {
        push(&mut context, second, std::ptr::null_mut());
    }
    let stats = context.object_heap_stats();
    assert_eq!(stats.fresh_blocks, fresh, "no new arena blocks");
    assert!(stats.reused_blocks >= 9);
}
