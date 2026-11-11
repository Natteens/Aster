//! MEM-EXP-1 stage C, U8: runtime glue for `List<T>` element ownership.
#![cfg(feature = "mem-exp-rc")]
#![allow(
    unsafe_code,
    clippy::cast_ptr_alignment,
    clippy::many_single_char_names
)]

use aster_runtime::object_heap::{IMMORTAL, ObjectDescriptor, strong_count};
use aster_runtime::{
    ASTER_ARRAY_DATA_OFFSET, AsterArray, AsterList, ExecutionContext, aster_rt_list_add,
    aster_rt_list_add_range, aster_rt_list_clear, aster_rt_list_get_range, aster_rt_list_new,
    aster_rt_list_remove_at, aster_rt_list_remove_range, aster_rt_list_set,
    aster_rt_list_set_element_descriptor, aster_rt_list_to_array,
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
