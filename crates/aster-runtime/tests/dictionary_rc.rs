//! MEM-EXP-1 stage C, U8: counted dictionaries (design section 11): uniform
//! header, key/value ownership glue, growth and release into the size-class
//! heap.
#![cfg(feature = "mem-exp-rc")]
#![allow(
    unsafe_code,
    clippy::cast_ptr_alignment,
    clippy::many_single_char_names
)]

use aster_runtime::object_heap::{
    DICTIONARY_FLAG, DictionaryDescriptors, IMMORTAL, KIND_DICTIONARY, KIND_MASK, KIND_SHIFT,
    ObjectDescriptor, STRING_ELEMENT_DESCRIPTOR, strong_count,
};
use aster_runtime::string::aster_rt_string_concat;
use aster_runtime::{
    ASTER_DICTIONARY_COUNT_OFFSET, ASTER_DICTIONARY_DESCRIPTOR_OFFSET, AsterDictionary,
    AsterStrHeader, ExecutionContext, aster_rt_dictionary_add, aster_rt_dictionary_clear,
    aster_rt_dictionary_contains_key, aster_rt_dictionary_get_or, aster_rt_dictionary_length,
    aster_rt_dictionary_new, aster_rt_dictionary_new_temporary, aster_rt_dictionary_remove,
    aster_rt_dictionary_set, aster_rt_dictionary_set_descriptors, aster_rt_temporary_scope_enter,
    encode_str,
};

const KEY_TYPE_KEY: i64 = 11;
const VALUE_TYPE_KEY: i64 = 22;
const KIND_INT: i32 = 7;
const KIND_STRING_KEY: i32 = 11;

static LEAF: ObjectDescriptor = ObjectDescriptor {
    size: 16,
    reference_count: 0,
    reference_offsets: std::ptr::null(),
};

// A value (or key) of one leaf at offset 0: a class object or a string.
static ONE_OFFSET: [u32; 1] = [0];
static ONE_LEAF: ObjectDescriptor = ObjectDescriptor {
    size: 8,
    reference_count: 1,
    reference_offsets: ONE_OFFSET.as_ptr(),
};

static VALUE_ONLY: DictionaryDescriptors = DictionaryDescriptors {
    key: std::ptr::null(),
    value: &raw const ONE_LEAF,
};
static KEY_AND_VALUE: DictionaryDescriptors = DictionaryDescriptors {
    key: &raw const STRING_ELEMENT_DESCRIPTOR,
    value: &raw const ONE_LEAF,
};

/// 8-byte-aligned backing store for a static (immortal) string.
fn aligned(value: &str) -> Vec<u64> {
    let bytes = encode_str(value);
    let mut buffer = vec![0_u64; bytes.len().div_ceil(8)];
    unsafe {
        std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), bytes.len())
            .copy_from_slice(&bytes);
    }
    buffer
}

fn count(object: *const u8) -> u32 {
    unsafe { strong_count(object.cast_mut()) }
}

#[allow(clippy::struct_field_names)]
struct Dict {
    context: Box<ExecutionContext>,
    dict: *mut AsterDictionary,
    kind: i32,
    key_size: i32,
}

impl Dict {
    fn new(kind: i32, pair: Option<&'static DictionaryDescriptors>) -> Self {
        let mut context = Box::new(ExecutionContext::with_stats());
        let cx: *mut ExecutionContext = &raw mut *context;
        let key_size = if kind == KIND_STRING_KEY { 8 } else { 4 };
        let dict = aster_rt_dictionary_new(
            cx,
            kind,
            key_size,
            key_size,
            KEY_TYPE_KEY,
            8,
            8,
            VALUE_TYPE_KEY,
        );
        assert!(!dict.is_null());
        if let Some(pair) = pair {
            aster_rt_dictionary_set_descriptors(cx, dict, std::ptr::from_ref(pair));
        }
        Self {
            context,
            dict,
            kind,
            key_size,
        }
    }

    fn ints(pair: Option<&'static DictionaryDescriptors>) -> Self {
        Self::new(KIND_INT, pair)
    }

    fn strings(pair: Option<&'static DictionaryDescriptors>) -> Self {
        Self::new(KIND_STRING_KEY, pair)
    }

    fn cx(&mut self) -> *mut ExecutionContext {
        &raw mut *self.context
    }

    /// A counted value object whose only count is the one handed over.
    fn object(&mut self) -> *mut u8 {
        let object = self.context.allocate_heap_object(16);
        unsafe {
            object
                .cast::<*const ObjectDescriptor>()
                .write(&raw const LEAF);
        }
        object
    }

    /// A fresh counted string of count 1.
    fn string(&mut self, text: &str) -> *const AsterStrHeader {
        // Two non-empty halves, so the concatenation is a fresh owned string.
        let (head, tail) = text.split_at(text.len() / 2);
        assert!(!head.is_empty() && !tail.is_empty());
        let (head, tail) = (aligned(head), aligned(tail));
        let cx = self.cx();
        let string = aster_rt_string_concat(cx, head.as_ptr().cast(), tail.as_ptr().cast());
        assert_eq!(count(string.cast()), 1);
        string
    }

    fn released(&self) -> u64 {
        self.context.object_heap_stats().released_blocks
    }

    fn length(&mut self) -> i32 {
        let cx = self.cx();
        aster_rt_dictionary_length(cx, self.dict)
    }

    fn add_or_set(&mut self, key: *const u8, value: *mut u8, replace: bool) -> bool {
        let cx = self.cx();
        let dict = self.dict;
        let slot = [value];
        let function = if replace {
            aster_rt_dictionary_set
        } else {
            aster_rt_dictionary_add
        };
        let result = function(
            cx,
            dict,
            self.kind,
            self.key_size,
            self.key_size,
            KEY_TYPE_KEY,
            8,
            8,
            VALUE_TYPE_KEY,
            key,
            slot.as_ptr().cast(),
        );
        assert!(result != 0, "runtime reported a failure");
        result == 2
    }

    fn add_int(&mut self, key: i32, value: *mut u8) -> bool {
        self.add_or_set((&raw const key).cast(), value, false)
    }

    fn set_int(&mut self, key: i32, value: *mut u8) -> bool {
        self.add_or_set((&raw const key).cast(), value, true)
    }

    fn add_string(&mut self, key: *const AsterStrHeader, value: *mut u8) -> bool {
        self.add_or_set((&raw const key).cast(), value, false)
    }

    fn set_string(&mut self, key: *const AsterStrHeader, value: *mut u8) -> bool {
        self.add_or_set((&raw const key).cast(), value, true)
    }

    fn get_or(&mut self, key: *const u8, supplied: *mut u8, add_missing: bool) -> *mut u8 {
        let cx = self.cx();
        let dict = self.dict;
        let supplied_slot = [supplied];
        let mut out: [*mut u8; 1] = [std::ptr::null_mut()];
        let ok = aster_rt_dictionary_get_or(
            cx,
            dict,
            self.kind,
            self.key_size,
            self.key_size,
            KEY_TYPE_KEY,
            8,
            8,
            VALUE_TYPE_KEY,
            key,
            supplied_slot.as_ptr().cast(),
            out.as_mut_ptr().cast(),
            i8::from(add_missing),
        );
        assert_eq!(ok, 1);
        out[0]
    }

    fn remove_int(&mut self, key: i32) -> bool {
        let cx = self.cx();
        let dict = self.dict;
        let result = aster_rt_dictionary_remove(
            cx,
            dict,
            self.kind,
            self.key_size,
            self.key_size,
            KEY_TYPE_KEY,
            8,
            8,
            VALUE_TYPE_KEY,
            (&raw const key).cast(),
        );
        assert!(result != 0);
        result == 2
    }

    fn contains_string(&mut self, key: *const AsterStrHeader) -> bool {
        let cx = self.cx();
        let dict = self.dict;
        let result = aster_rt_dictionary_contains_key(
            cx,
            dict,
            self.kind,
            self.key_size,
            self.key_size,
            KEY_TYPE_KEY,
            8,
            8,
            VALUE_TYPE_KEY,
            (&raw const key).cast(),
        );
        assert!(result != 0);
        result == 2
    }

    fn clear(&mut self) {
        let cx = self.cx();
        let dict = self.dict;
        assert_eq!(
            aster_rt_dictionary_clear(
                cx,
                dict,
                self.kind,
                self.key_size,
                self.key_size,
                KEY_TYPE_KEY,
                8,
                8,
                VALUE_TYPE_KEY,
            ),
            1
        );
    }

    fn drop_dictionary(&mut self) {
        unsafe { self.context.release_counted_object(self.dict.cast()) };
    }
}

#[test]
fn header_has_the_uniform_prefix_and_kind() {
    assert_eq!(ASTER_DICTIONARY_DESCRIPTOR_OFFSET, 0);
    assert_eq!(ASTER_DICTIONARY_COUNT_OFFSET, 8);
    assert_eq!(KIND_DICTIONARY, 4);
    assert_eq!(DICTIONARY_FLAG, 4 << KIND_SHIFT);
    let d = Dict::ints(Some(&VALUE_ONLY));
    let descriptor = unsafe { d.dict.cast::<*const DictionaryDescriptors>().read() };
    assert_eq!(descriptor, &raw const VALUE_ONLY);
    let word = unsafe {
        d.dict
            .cast::<u8>()
            .add(ASTER_DICTIONARY_COUNT_OFFSET)
            .cast::<u32>()
            .read()
    };
    assert_eq!(word, 1 | DICTIONARY_FLAG);
    assert_eq!(word & KIND_MASK, DICTIONARY_FLAG);
    // The header is a 96-byte size-class block.
    assert_eq!(d.context.object_heap_stats().fresh_blocks, 1);
}

#[test]
fn add_new_keeps_both_counts_and_existing_releases_the_passed_value() {
    let mut d = Dict::ints(Some(&VALUE_ONLY));
    let (a, b) = (d.object(), d.object());
    assert!(d.add_int(1, a));
    assert_eq!(count(a), 1);
    assert_eq!(d.length(), 1);
    let before = d.released();
    // The key exists: nothing is inserted and the passed value is dropped.
    assert!(!d.add_int(1, b));
    assert_eq!(d.length(), 1);
    assert_eq!(d.released(), before + 1);
    assert_eq!(count(a), 1);
    // `b` was the passed value: its block is free and is handed out again.
    assert_eq!(d.object(), b);
}

#[test]
fn set_new_keeps_counts_and_existing_releases_the_old_value() {
    let mut d = Dict::ints(Some(&VALUE_ONLY));
    let (a, b) = (d.object(), d.object());
    // Set reports whether it replaced an existing value.
    assert!(!d.set_int(7, a));
    assert_eq!(d.length(), 1);
    assert_eq!(d.released(), 0);
    // Set on an existing key replaces the value and drops the old one.
    assert!(d.set_int(7, b));
    assert_eq!(d.length(), 1);
    assert_eq!(d.released(), 1);
    assert_eq!(count(b), 1);
    assert_eq!(d.object(), a);
    // Replacing with the same object (the pass retained it first) keeps it.
    unsafe { d.context.retain_object(b) };
    let before = d.released();
    assert!(d.set_int(7, b));
    assert_eq!(count(b), 1);
    assert_eq!(d.released(), before);
}

#[test]
fn set_on_an_existing_key_releases_the_passed_key_too() {
    let mut d = Dict::strings(Some(&KEY_AND_VALUE));
    let first = d.string("alpha");
    let (a, b) = (d.object(), d.object());
    assert!(!d.set_string(first, a));
    assert_eq!(count(first.cast()), 1);
    // A second string with the same text: the entry keeps `first`, the passed
    // key (count 1, consumed) is released, and so is the old value.
    let second = d.string("alpha");
    let strings_before = d.context.object_heap_stats().released_strings;
    assert!(d.set_string(second, b));
    let stats = d.context.object_heap_stats();
    assert_eq!(stats.released_strings, strings_before + 1);
    assert_eq!(d.length(), 1);
    assert_eq!(count(first.cast()), 1);
    assert_eq!(count(b), 1);
    // The old value `a` and the passed key `second` are free blocks again.
    let reused = [d.object(), d.object()];
    assert!(reused.contains(&a));
}

#[test]
fn get_or_add_miss_inserts_and_hit_drops_the_passed_operands() {
    let mut d = Dict::strings(Some(&KEY_AND_VALUE));
    let key = d.string("kk");
    let (a, b) = (d.object(), d.object());
    let key_slot = [key];
    // Miss: the entry takes the passed key and value; the result is `a`.
    let got = d.get_or(key_slot.as_ptr().cast(), a, true);
    assert_eq!(got, a);
    assert_eq!(d.length(), 1);
    assert_eq!((count(key.cast()), count(a)), (1, 1));
    // Hit: the passed key and value are released, the stored value returns.
    let again = d.string("kk");
    let again_slot = [again];
    let before = d.context.object_heap_stats();
    let got = d.get_or(again_slot.as_ptr().cast(), b, true);
    assert_eq!(got, a);
    assert_eq!(d.length(), 1);
    let after = d.context.object_heap_stats();
    assert_eq!(after.released_strings, before.released_strings + 1);
    assert_eq!(after.released_blocks, before.released_blocks + 2);
    assert_eq!(count(a), 1);
}

#[test]
fn plain_get_or_does_not_touch_counts() {
    let mut d = Dict::ints(Some(&VALUE_ONLY));
    let (a, fallback) = (d.object(), d.object());
    assert!(d.add_int(3, a));
    let before = d.context.object_heap_stats();
    let key = 3_i32;
    assert_eq!(d.get_or((&raw const key).cast(), fallback, false), a);
    let missing = 4_i32;
    assert_eq!(
        d.get_or((&raw const missing).cast(), fallback, false),
        fallback
    );
    assert_eq!(d.length(), 1);
    assert_eq!(d.context.object_heap_stats(), before);
    assert_eq!((count(a), count(fallback)), (1, 1));
}

#[test]
fn remove_releases_the_removed_value_after_unlinking() {
    let mut d = Dict::ints(Some(&VALUE_ONLY));
    let (a, b) = (d.object(), d.object());
    assert!(d.add_int(1, a));
    assert!(d.add_int(2, b));
    assert!(!d.remove_int(99));
    assert_eq!(d.released(), 0);
    assert!(d.remove_int(1));
    assert_eq!(d.length(), 1);
    assert_eq!(d.released(), 1);
    assert_eq!(count(b), 1);
    // The removed key is really gone; the other still resolves to `b`.
    let key = 2_i32;
    assert_eq!(d.get_or((&raw const key).cast(), a, false), b);
    assert!(!d.remove_int(1));
    assert_eq!(d.released(), 1);
}

#[test]
fn tombstones_own_nothing_in_clear_drop_and_compaction() {
    let mut d = Dict::ints(Some(&VALUE_ONLY));
    let values: Vec<*mut u8> = (0..6).map(|_| d.object()).collect();
    for (key, value) in values.iter().enumerate() {
        assert!(d.add_int(i32::try_from(key).unwrap(), *value));
    }
    // Remove three: they become tombstones and their values are released once.
    for key in 0..3 {
        assert!(d.remove_int(key));
    }
    assert_eq!(d.released(), 3);
    // Fill the entry buffer so the next insert compacts it in place.
    let extra: Vec<*mut u8> = (0..3).map(|_| d.object()).collect();
    for (offset, value) in extra.iter().enumerate() {
        assert!(d.add_int(10 + i32::try_from(offset).unwrap(), *value));
    }
    assert_eq!(d.length(), 6);
    // Compaction moved bits only: every survivor still has exactly one count.
    for value in values[3..].iter().chain(&extra) {
        assert_eq!(count(*value), 1);
    }
    let after_compaction = d.released();
    // The compacted table no longer references the removed values.
    d.clear();
    assert_eq!(d.released(), after_compaction + 6);
    assert_eq!(d.length(), 0);
    d.drop_dictionary();
    // Nothing was released twice: only storage and header went back.
    assert_eq!(d.released(), after_compaction + 6 + 2);
}

#[test]
fn clear_releases_every_former_value_and_keeps_the_table_usable() {
    let mut d = Dict::ints(Some(&VALUE_ONLY));
    let values: Vec<*mut u8> = (0..5).map(|_| d.object()).collect();
    for (key, value) in values.iter().enumerate() {
        assert!(d.add_int(i32::try_from(key).unwrap(), *value));
    }
    d.clear();
    assert_eq!(d.length(), 0);
    assert_eq!(d.released(), 5);
    let again = d.object();
    assert!(values.contains(&again));
    assert!(d.add_int(0, again));
    assert_eq!(d.length(), 1);
    // Clearing an empty dictionary releases nothing.
    d.clear();
    let after = d.released();
    d.clear();
    assert_eq!(d.released(), after);
}

#[test]
fn drop_releases_live_keys_values_storage_and_header() {
    let mut d = Dict::strings(Some(&KEY_AND_VALUE));
    let (a, b) = (d.object(), d.object());
    let (k1, k2) = (d.string("one"), d.string("two"));
    assert!(d.add_string(k1, a));
    assert!(d.add_string(k2, b));
    // A second holder of `b` outlives the dictionary.
    unsafe { d.context.retain_object(b) };
    d.drop_dictionary();
    let stats = d.context.object_heap_stats();
    assert_eq!(stats.released_dictionaries, 1);
    assert_eq!(stats.released_strings, 2);
    // `a`, two strings, the storage block and the header; `b` survives.
    assert_eq!(stats.released_blocks, 5);
    assert_eq!(count(b), 1);
    unsafe { d.context.release_counted_object(b) };
    assert_eq!(d.released(), 6);
}

#[test]
fn growth_returns_the_old_storage_block_and_it_is_reused() {
    let mut d = Dict::ints(None);
    // Initial storage is 8 buckets + 8 entries (288 bytes); the seventh entry
    // grows the table to 16 + 16 and returns the first block.
    for key in 0..6 {
        assert!(d.add_int(key, std::ptr::null_mut()));
    }
    assert_eq!(d.released(), 0);
    assert!(d.add_int(6, std::ptr::null_mut()));
    assert_eq!(d.released(), 1);
    assert_eq!(d.length(), 7);
    // A second dictionary's first storage block reuses the returned one.
    let reused_before = d.context.object_heap_stats().reused_blocks;
    let cx = d.cx();
    let other = aster_rt_dictionary_new(cx, KIND_INT, 4, 4, KEY_TYPE_KEY, 8, 8, VALUE_TYPE_KEY);
    let slot = [std::ptr::null_mut::<u8>()];
    let key = 1_i32;
    assert_eq!(
        aster_rt_dictionary_add(
            cx,
            other,
            KIND_INT,
            4,
            4,
            KEY_TYPE_KEY,
            8,
            8,
            VALUE_TYPE_KEY,
            (&raw const key).cast(),
            slot.as_ptr().cast(),
        ),
        2
    );
    assert!(d.context.object_heap_stats().reused_blocks > reused_before);
}

#[test]
fn storage_above_the_largest_class_stays_reserved() {
    let mut d = Dict::ints(None);
    // 100 entries need 256 buckets + 128 entries: above 2,048 bytes.
    for key in 0..100 {
        assert!(d.add_int(key, std::ptr::null_mut()));
    }
    let grown = d.context.object_heap_stats();
    d.drop_dictionary();
    let stats = d.context.object_heap_stats();
    assert_eq!(stats.unreturned_dictionary_blocks, 1);
    assert_eq!(stats.released_dictionaries, 1);
    // Only the header returns for the final storage.
    assert_eq!(stats.released_blocks, grown.released_blocks + 1);
    assert!(
        grown.released_blocks >= 2,
        "small blocks returned on growth"
    );
}

#[test]
fn string_keys_hash_by_content_with_counted_strings() {
    let mut d = Dict::strings(Some(&KEY_AND_VALUE));
    let stored = d.string("needle");
    let value = d.object();
    assert!(d.add_string(stored, value));
    // A different string object with the same text finds the entry, whether
    // it is counted or static.
    let probe = d.string("needle");
    assert!(d.contains_string(probe));
    let static_text = aligned("needle");
    assert!(d.contains_string(static_text.as_ptr().cast()));
    let other = aligned("haystack");
    assert!(!d.contains_string(other.as_ptr().cast()));
    // The probes were borrowed: no count changed.
    assert_eq!((count(stored.cast()), count(probe.cast())), (1, 1));
    // Content equality also detects a duplicate on Add.
    let spare = d.object();
    let duplicate = d.string("needle");
    assert!(!d.add_string(duplicate, spare));
    assert_eq!(d.length(), 1);
    // A hundred distinct strings still resolve after several rebuilds.
    for index in 0..100 {
        let key = d.string(&format!("key-{index}"));
        let object = d.object();
        assert!(d.add_string(key, object));
    }
    for index in 0..100 {
        let text = aligned(&format!("key-{index}"));
        assert!(d.contains_string(text.as_ptr().cast()));
    }
    assert_eq!(d.length(), 101);
}

#[test]
fn temporary_dictionaries_are_immortal_but_still_own_their_entries() {
    let mut context = Box::new(ExecutionContext::with_stats());
    let cx: *mut ExecutionContext = &raw mut *context;
    aster_rt_temporary_scope_enter(cx);
    let dict =
        aster_rt_dictionary_new_temporary(cx, KIND_INT, 4, 4, KEY_TYPE_KEY, 8, 8, VALUE_TYPE_KEY);
    assert_eq!(count(dict.cast()), IMMORTAL);
    let word = unsafe {
        dict.cast::<u8>()
            .add(ASTER_DICTIONARY_COUNT_OFFSET)
            .cast::<u32>()
            .read()
    };
    assert_eq!(word, IMMORTAL | DICTIONARY_FLAG);
    aster_rt_dictionary_set_descriptors(cx, dict, &raw const VALUE_ONLY);
    unsafe {
        context.retain_object(dict.cast());
        context.release_counted_object(dict.cast());
    }
    assert_eq!(count(dict.cast()), IMMORTAL);
    assert_eq!(context.object_heap_stats().released_blocks, 0);
    // Entries put into a temporary dictionary are still owned counts.
    let value = context.allocate_heap_object(16);
    unsafe {
        value
            .cast::<*const ObjectDescriptor>()
            .write(&raw const LEAF);
    }
    let slot = [value];
    let key = 5_i32;
    assert_eq!(
        aster_rt_dictionary_add(
            cx,
            dict,
            KIND_INT,
            4,
            4,
            KEY_TYPE_KEY,
            8,
            8,
            VALUE_TYPE_KEY,
            (&raw const key).cast(),
            slot.as_ptr().cast(),
        ),
        2
    );
    assert_eq!(
        aster_rt_dictionary_remove(
            cx,
            dict,
            KIND_INT,
            4,
            4,
            KEY_TYPE_KEY,
            8,
            8,
            VALUE_TYPE_KEY,
            (&raw const key).cast(),
        ),
        2
    );
    assert_eq!(context.object_heap_stats().released_blocks, 1);
}

#[test]
fn a_dictionary_without_descriptors_does_no_counting() {
    let mut d = Dict::ints(None);
    let value = d.object();
    assert!(d.add_int(1, value));
    assert!(d.set_int(1, value));
    assert!(d.remove_int(1));
    assert!(d.add_int(2, value));
    d.clear();
    assert_eq!(count(value), 1);
    let stats = d.context.object_heap_stats();
    assert_eq!((stats.retains, stats.releases), (0, 0));
}

#[test]
fn immortal_and_null_leaves_are_ignored() {
    let mut d = Dict::ints(Some(&VALUE_ONLY));
    let big = d.context.allocate_heap_object(4096);
    assert_eq!(count(big), IMMORTAL);
    assert!(d.add_int(1, big));
    assert!(d.add_int(2, std::ptr::null_mut()));
    assert!(d.set_int(1, std::ptr::null_mut()));
    assert!(d.remove_int(2));
    d.clear();
    let stats = d.context.object_heap_stats();
    assert_eq!((stats.retains, stats.releases), (0, 0));
    assert_eq!(count(big), IMMORTAL);
}

#[test]
fn a_dictionary_of_dictionaries_is_released_without_recursion() {
    // Values that are themselves counted dictionaries: dropping the outer
    // chain must use the work list, not the native stack.
    static NESTED: ObjectDescriptor = ObjectDescriptor {
        size: 8,
        reference_count: 1,
        reference_offsets: ONE_OFFSET.as_ptr(),
    };
    static NESTED_PAIR: DictionaryDescriptors = DictionaryDescriptors {
        key: std::ptr::null(),
        value: &raw const NESTED,
    };
    let mut context = Box::new(ExecutionContext::with_stats());
    let cx: *mut ExecutionContext = &raw mut *context;
    let depth = 50_000_u64;
    let new = |cx: *mut ExecutionContext| {
        let dict = aster_rt_dictionary_new(cx, KIND_INT, 4, 4, KEY_TYPE_KEY, 8, 8, VALUE_TYPE_KEY);
        aster_rt_dictionary_set_descriptors(cx, dict, &raw const NESTED_PAIR);
        dict
    };
    let mut inner = new(cx);
    for _ in 0..depth {
        let outer = new(cx);
        let slot = [inner];
        let key = 0_i32;
        assert_eq!(
            aster_rt_dictionary_add(
                cx,
                outer,
                KIND_INT,
                4,
                4,
                KEY_TYPE_KEY,
                8,
                8,
                VALUE_TYPE_KEY,
                (&raw const key).cast(),
                slot.as_ptr().cast(),
            ),
            2
        );
        inner = outer;
    }
    unsafe { context.release_counted_object(inner.cast()) };
    let stats = context.object_heap_stats();
    assert_eq!(stats.released_dictionaries, depth + 1);
}
