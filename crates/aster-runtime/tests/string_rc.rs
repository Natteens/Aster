//! MEM-EXP-1 stage C, U8: counted strings (uniform header, STRING flag, owned
//! results, release into the size-class heap).
#![cfg(feature = "mem-exp-rc")]
#![allow(unsafe_code, clippy::cast_ptr_alignment)]

use aster_runtime::object_heap::{ARRAY_FLAG, IMMORTAL, STRING_FLAG, strong_count};
use aster_runtime::string::{
    aster_rt_string_concat, aster_rt_string_concat_temporary, aster_rt_string_from_long,
    aster_rt_string_repeat, aster_rt_string_replace, aster_rt_string_split,
    aster_rt_string_substring_from, aster_rt_string_substring_range, aster_rt_string_trim,
    aster_rt_string_trim_end, aster_rt_string_trim_start,
};
use aster_runtime::{
    ASTER_STR_COUNT_OFFSET, ASTER_STR_LEN_OFFSET, ASTER_STR_PAYLOAD_OFFSET, AsterArray,
    AsterStrHeader, ExecutionContext, aster_rt_array_element, aster_rt_temporary_scope_enter,
    decode_str, encode_str,
};

/// 8-byte-aligned backing store for a static string.
fn aligned(value: &str) -> Vec<u64> {
    let bytes = encode_str(value);
    let mut buffer = vec![0_u64; bytes.len().div_ceil(8)];
    unsafe {
        std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), bytes.len())
            .copy_from_slice(&bytes);
    }
    buffer
}

fn ptr_of(buffer: &[u64]) -> *const AsterStrHeader {
    buffer.as_ptr().cast()
}

fn word(string: *const AsterStrHeader) -> u32 {
    unsafe {
        string
            .cast::<u8>()
            .add(ASTER_STR_COUNT_OFFSET)
            .cast::<u32>()
            .read()
    }
}

fn count(string: *const AsterStrHeader) -> u32 {
    unsafe { strong_count(string.cast_mut().cast()) }
}

fn text(string: *const AsterStrHeader) -> String {
    unsafe { decode_str(string) }.expect("valid string")
}

fn release(cx: &mut ExecutionContext, string: *const AsterStrHeader) {
    unsafe { cx.release_counted_object(string.cast_mut().cast()) };
}

#[test]
fn header_layout_is_uniform() {
    assert_eq!(ASTER_STR_COUNT_OFFSET, 8);
    assert_eq!(ASTER_STR_LEN_OFFSET, 16);
    assert_eq!(ASTER_STR_PAYLOAD_OFFSET, 24);
    assert_eq!(size_of::<AsterStrHeader>(), 24);
    let encoded = encode_str("abc");
    assert_eq!(encoded.len(), 27);
    assert_eq!(&encoded[0..8], &[0; 8]);
    assert_eq!(
        encoded[8..12],
        (IMMORTAL | STRING_FLAG).to_ne_bytes(),
        "static strings are immortal and flagged"
    );
    assert_eq!(encoded[16..24], 3_usize.to_ne_bytes());
    assert_eq!(&encoded[24..], b"abc");
    assert_ne!(STRING_FLAG, ARRAY_FLAG);
}

#[test]
fn count_initialization_fresh_temporary_and_static() {
    let mut cx = ExecutionContext::with_stats();
    let a = aligned("a");
    let b = aligned("b");
    let fresh = aster_rt_string_concat(&raw mut cx, ptr_of(&a), ptr_of(&b));
    assert_eq!(text(fresh), "ab");
    assert_eq!(count(fresh), 1);
    assert_eq!(word(fresh) & STRING_FLAG, STRING_FLAG);

    aster_rt_temporary_scope_enter(&raw mut cx);
    let temporary = aster_rt_string_concat_temporary(&raw mut cx, ptr_of(&a), ptr_of(&b));
    assert_eq!(text(temporary), "ab");
    assert_eq!(count(temporary), IMMORTAL);
    assert_eq!(word(temporary) & STRING_FLAG, STRING_FLAG);

    assert_eq!(count(ptr_of(&a)), IMMORTAL);
    assert_eq!(word(ptr_of(&a)) & STRING_FLAG, STRING_FLAG);
    // Releasing immortal strings is a no-op.
    let before = cx.object_heap_stats();
    release(&mut cx, temporary);
    release(&mut cx, ptr_of(&a));
    assert_eq!(cx.object_heap_stats(), before);
}

#[test]
fn display_strings_are_counted() {
    let mut cx = ExecutionContext::with_stats();
    let number = aster_rt_string_from_long(&raw mut cx, 12345);
    assert_eq!(text(number), "12345");
    assert_eq!(count(number), 1);
    release(&mut cx, number);
    assert_eq!(cx.object_heap_stats().released_strings, 1);
}

#[test]
fn release_returns_the_block_and_it_is_reused() {
    let mut cx = ExecutionContext::with_stats();
    let a = aligned("hello ");
    let b = aligned("world");
    let first = aster_rt_string_concat(&raw mut cx, ptr_of(&a), ptr_of(&b));
    let after_first = cx.object_heap_stats();
    assert_eq!(after_first.fresh_blocks, 1);
    release(&mut cx, first);
    let released = cx.object_heap_stats();
    assert_eq!(released.released_blocks, 1);
    assert_eq!(released.released_strings, 1);
    let second = aster_rt_string_concat(&raw mut cx, ptr_of(&b), ptr_of(&a));
    assert_eq!(text(second), "worldhello ");
    let reused = cx.object_heap_stats();
    assert_eq!(reused.reused_blocks, 1);
    assert_eq!(reused.fresh_blocks, 1);
    assert_eq!(second, first, "same size class, same block");
}

#[test]
fn string_loop_reclaims_blocks() {
    let mut cx = ExecutionContext::with_stats();
    let x = aligned("x");
    let mut current = aster_rt_string_concat(&raw mut cx, ptr_of(&x), ptr_of(&x));
    for _ in 0..200 {
        let next = aster_rt_string_concat(&raw mut cx, current, ptr_of(&x));
        release(&mut cx, current);
        current = next;
    }
    assert_eq!(text(current).len(), 202);
    let stats = cx.object_heap_stats();
    assert!(stats.reused_blocks >= 150, "{stats:?}");
    assert!(stats.fresh_blocks < 40, "{stats:?}");
}

#[test]
fn entry_points_return_owned_strings_even_on_noop_paths() {
    let mut cx = ExecutionContext::with_stats();
    let a = aligned("  padded  ");
    let b = aligned("x");
    let empty = aligned("");
    let source = aster_rt_string_concat(&raw mut cx, ptr_of(&a), ptr_of(&empty));
    assert_eq!(count(source), 1);
    let plain = aster_rt_string_concat(&raw mut cx, ptr_of(&b), ptr_of(&empty));
    let cx_ptr = &raw mut cx;
    let results = [
        aster_rt_string_concat(cx_ptr, source, ptr_of(&empty)),
        aster_rt_string_concat(cx_ptr, ptr_of(&empty), source),
        aster_rt_string_substring_from(cx_ptr, source, 0),
        aster_rt_string_substring_range(cx_ptr, source, 0, 10),
        aster_rt_string_trim(cx_ptr, plain),
        aster_rt_string_trim_start(cx_ptr, plain),
        aster_rt_string_trim_end(cx_ptr, plain),
        aster_rt_string_replace(cx_ptr, plain, ptr_of(&a), ptr_of(&b)),
        aster_rt_string_repeat(cx_ptr, plain, 1),
    ];
    for result in results {
        assert!(!result.is_null());
        // A result is either an independent count-1 string or the argument
        // itself with its count raised (no entry point returns a borrowed one).
        let expected = if result == source || result == plain {
            2
        } else {
            1
        };
        assert_eq!(count(result), expected);
        assert_ne!(word(result) & STRING_FLAG, 0);
    }
    // The sources stay valid after every result has been released.
    for result in results {
        release(&mut cx, result);
    }
    release(&mut cx, source);
    assert_eq!(text(plain), "x");
    release(&mut cx, plain);
}

#[test]
fn substring_results_survive_releasing_the_source() {
    let mut cx = ExecutionContext::with_stats();
    let a = aligned("  hello world  ");
    let empty = aligned("");
    let source = aster_rt_string_concat(&raw mut cx, ptr_of(&a), ptr_of(&empty));
    let trimmed = aster_rt_string_trim(&raw mut cx, source);
    let part = aster_rt_string_substring_range(&raw mut cx, trimmed, 6, 5);
    release(&mut cx, source);
    release(&mut cx, trimmed);
    // Churn the heap so a dangling result would be overwritten.
    for _ in 0..8 {
        let junk = aster_rt_string_concat(&raw mut cx, ptr_of(&a), ptr_of(&a));
        release(&mut cx, junk);
    }
    assert_eq!(text(part), "world");
    assert_eq!(count(part), 1);
}

#[test]
fn split_array_releases_its_strings() {
    let mut cx = ExecutionContext::with_stats();
    let joined = aligned("a,b,c,d");
    let comma = aligned(",");
    let array: *mut AsterArray =
        aster_rt_string_split(&raw mut cx, ptr_of(&joined), ptr_of(&comma));
    assert!(!array.is_null());
    let mut elements = Vec::new();
    for index in 0..4 {
        let slot = aster_rt_array_element(&raw mut cx, array, index);
        let item = unsafe { slot.cast::<*const AsterStrHeader>().read_unaligned() };
        assert_eq!(count(item), 1);
        elements.push(text(item));
    }
    assert_eq!(elements, ["a", "b", "c", "d"]);
    let before = cx.object_heap_stats();
    unsafe { cx.release_counted_object(array.cast()) };
    let after = cx.object_heap_stats();
    assert_eq!(after.released_arrays - before.released_arrays, 1);
    assert_eq!(after.released_strings - before.released_strings, 4);
    // Header, data block and four strings went back to the heap.
    assert_eq!(after.released_blocks - before.released_blocks, 6);
}

#[test]
fn strings_above_one_mib_are_immortal_and_never_returned() {
    let mut cx = ExecutionContext::with_stats();
    let empty = aligned("");
    // The boundary: header plus payload of exactly 1 MiB is still counted.
    let payload = "y".repeat(1_048_576 - ASTER_STR_PAYLOAD_OFFSET);
    let at_limit = aligned(&payload);
    let counted = aster_rt_string_concat(&raw mut cx, ptr_of(&at_limit), ptr_of(&empty));
    assert_eq!(count(counted), 1);
    let beyond = aligned(&format!("{payload}y"));
    let over = aster_rt_string_concat(&raw mut cx, ptr_of(&beyond), ptr_of(&empty));
    assert_eq!(count(over), IMMORTAL);
    assert_eq!(word(over) & STRING_FLAG, STRING_FLAG);
    let before = cx.object_heap_stats();
    release(&mut cx, over);
    assert_eq!(cx.object_heap_stats(), before);
}

#[test]
fn long_strings_are_counted_and_reused() {
    let mut cx = ExecutionContext::with_stats();
    let empty = aligned("");
    let body = aligned(&"z".repeat(10 * 1024));
    let long = aster_rt_string_concat(&raw mut cx, ptr_of(&body), ptr_of(&empty));
    assert_eq!(text(long).len(), 10 * 1024);
    assert_eq!(count(long), 1);
    assert_eq!(word(long) & STRING_FLAG, STRING_FLAG);
    let fresh = cx.object_heap_stats().fresh_blocks;
    release(&mut cx, long);
    let stats = cx.object_heap_stats();
    assert_eq!(stats.released_strings, 1);
    assert_eq!(stats.released_blocks, 1);
    // The next string of the same class reuses the 16 KiB block, zeroed.
    let again = aster_rt_string_concat(&raw mut cx, ptr_of(&body), ptr_of(&empty));
    assert_eq!(text(again), "z".repeat(10 * 1024));
    let stats = cx.object_heap_stats();
    assert_eq!(stats.fresh_blocks, fresh);
    assert_eq!(stats.reused_blocks, 1);
}
