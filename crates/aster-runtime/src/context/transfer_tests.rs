//! Unit tests for deep-copy serialization and materialization (CONC-005).

#![allow(
    unsafe_code,
    clippy::cast_ptr_alignment,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use super::*;
use crate::transfer::{TransferCase, TransferShape};

fn string_array(context: &mut ExecutionContext, values: &[&str]) -> *mut AsterArray {
    let array = context.allocate_array(values.len() as i32, 8);
    assert!(!array.is_null());
    #[cfg(feature = "mem-exp-rc")]
    ExecutionContext::set_string_array_descriptor(array);
    for (index, value) in values.iter().enumerate() {
        let string = context.allocate_string_parts(&[value]);
        unsafe {
            (*array)
                .data
                .add(index * 8)
                .cast::<*const AsterStrHeader>()
                .write(string);
        }
    }
    array
}

fn read_strings(array: *const AsterArray) -> Vec<String> {
    unsafe {
        (0..(*array).length as usize)
            .map(|index| {
                let string = (*array)
                    .data
                    .add(index * 8)
                    .cast::<*const AsterStrHeader>()
                    .read();
                crate::string::decode_str(string).expect("valid string")
            })
            .collect()
    }
}

fn round_trip(
    source: &mut ExecutionContext,
    shape: &TransferShape,
    image: *const u8,
    destination: &mut ExecutionContext,
    out: *mut u8,
) {
    let shape = TransferShape::decode(&shape.encode()).expect("shape decodes");
    let wire = unsafe { source.transfer_pack(&shape, image) }.expect("packs");
    unsafe { destination.transfer_unpack(&shape, wire.as_bytes(), out) }.expect("unpacks");
}

#[test]
fn string_array_copy_is_independent_of_the_source() {
    let mut source = ExecutionContext::new();
    let mut destination = ExecutionContext::new();
    let array = string_array(&mut source, &["alpha", "", "gamma \u{2713}"]);
    let image = array as usize;
    let mut out = 0_usize;
    let shape = TransferShape::Array {
        element: Box::new(TransferShape::String),
        descriptor: 0,
    };
    round_trip(
        &mut source,
        &shape,
        (&raw const image).cast(),
        &mut destination,
        (&raw mut out).cast(),
    );
    let copy = out as *mut AsterArray;
    assert_ne!(copy as usize, array as usize);
    assert_eq!(read_strings(copy), ["alpha", "", "gamma \u{2713}"]);
    drop(source);
    assert_eq!(read_strings(copy), ["alpha", "", "gamma \u{2713}"]);
}

#[test]
fn empty_and_null_values_round_trip() {
    let mut source = ExecutionContext::new();
    let mut destination = ExecutionContext::new();
    let empty = source.allocate_array(0, 4);
    let shape = TransferShape::Array {
        element: Box::new(TransferShape::Scalar { size: 4 }),
        descriptor: 0,
    };
    let image = empty as usize;
    let mut out = 0_usize;
    round_trip(
        &mut source,
        &shape,
        (&raw const image).cast(),
        &mut destination,
        (&raw mut out).cast(),
    );
    assert_ne!(out, 0);
    assert_eq!(unsafe { (*(out as *const AsterArray)).length }, 0);
    let null = 0_usize;
    let mut out = 1_usize;
    round_trip(
        &mut source,
        &shape,
        (&raw const null).cast(),
        &mut destination,
        (&raw mut out).cast(),
    );
    assert_eq!(out, 0);
}

#[test]
fn list_and_dictionary_of_strings_round_trip_in_order() {
    let mut source = ExecutionContext::new();
    let mut destination = ExecutionContext::new();
    let list = source.allocate_list_in_region(8, 8, 77, ListRegion::Persistent);
    for value in ["x", "yy", "zzz"] {
        let string = source.allocate_string_parts(&[value]);
        source.list_add(list, 8, 8, 77, (&raw const string).cast());
    }
    let dictionary = source.allocate_dictionary_in_region(
        DictionaryKeyKind::String,
        8,
        8,
        1,
        4,
        4,
        2,
        ListRegion::Persistent,
    );
    for (key, value) in [("one", 1_i32), ("two", 2), ("three", 3)] {
        let key = source.allocate_string_parts(&[key]);
        source.dictionary_add_or_set(
            dictionary,
            DictionaryKeyKind::String,
            8,
            8,
            1,
            4,
            4,
            2,
            (&raw const key).cast(),
            (&raw const value).cast(),
            false,
        );
    }
    assert!(source.error.is_none(), "{:?}", source.error);
    let strings = TransferShape::String;
    let pair = TransferShape::Struct {
        size: 16,
        fields: vec![
            (
                0,
                TransferShape::List {
                    element: Box::new(strings.clone()),
                    descriptor: 0,
                },
            ),
            (
                8,
                TransferShape::Dictionary {
                    key: Box::new(strings),
                    value: Box::new(TransferShape::Scalar { size: 4 }),
                    descriptors: 0,
                },
            ),
        ],
    };
    let image = [list as usize, dictionary as usize];
    let mut out = [0_usize; 2];
    round_trip(
        &mut source,
        &pair,
        image.as_ptr().cast(),
        &mut destination,
        out.as_mut_ptr().cast(),
    );
    drop(source);
    let copy = out[0] as *const AsterList;
    unsafe {
        assert_eq!((*copy).length, 3);
        let texts = (0..3)
            .map(|index| {
                crate::string::decode_str(
                    (*copy)
                        .data
                        .add(index * 8)
                        .cast::<*const AsterStrHeader>()
                        .read(),
                )
                .expect("string")
            })
            .collect::<Vec<_>>();
        assert_eq!(texts, ["x", "yy", "zzz"]);
        let copy = out[1] as *const AsterDictionary;
        assert_eq!((*copy).length, 3);
    }
}

#[test]
fn enum_payload_selects_the_tagged_case_and_corrupt_data_is_rejected() {
    let mut source = ExecutionContext::new();
    let mut destination = ExecutionContext::new();
    let shape = TransferShape::Enum {
        size: 16,
        cases: vec![
            TransferCase {
                tag: 0,
                fields: Vec::new(),
            },
            TransferCase {
                tag: 1,
                fields: vec![(8, TransferShape::String)],
            },
        ],
    };
    let string = source.allocate_string_parts(&["payload"]);
    let mut image = [0_u64; 2];
    image[0] = 1;
    image[1] = string as usize as u64;
    let mut out = [0_u64; 2];
    round_trip(
        &mut source,
        &shape,
        image.as_ptr().cast(),
        &mut destination,
        out.as_mut_ptr().cast(),
    );
    assert_eq!(out[0], 1);
    let text = unsafe { crate::string::decode_str(out[1] as usize as *const AsterStrHeader) };
    assert_eq!(text.as_deref(), Some("payload"));
    assert_ne!(out[1], image[1]);

    let wire = unsafe { source.transfer_pack(&shape, image.as_ptr().cast()) }.expect("packs");
    let truncated = &wire.as_bytes()[..wire.len() - 1];
    let mut sink = [0_u64; 2];
    assert!(
        unsafe { destination.transfer_unpack(&shape, truncated, sink.as_mut_ptr().cast()) }
            .is_err()
    );
    let mut bad = wire.as_bytes().to_vec();
    bad[0] = 9;
    let mut destination = ExecutionContext::new();
    assert!(
        unsafe { destination.transfer_unpack(&shape, &bad, sink.as_mut_ptr().cast()) }.is_err()
    );
}

#[test]
fn large_array_of_scalars_round_trips() {
    let mut source = ExecutionContext::new();
    let mut destination = ExecutionContext::new();
    let length = 200_000_i32;
    let array = source.allocate_array(length, 4);
    unsafe {
        for index in 0..length {
            (*array)
                .data
                .add(index as usize * 4)
                .cast::<i32>()
                .write_unaligned(index.wrapping_mul(3));
        }
    }
    let shape = TransferShape::Array {
        element: Box::new(TransferShape::Scalar { size: 4 }),
        descriptor: 0,
    };
    let image = array as usize;
    let mut out = 0_usize;
    round_trip(
        &mut source,
        &shape,
        (&raw const image).cast(),
        &mut destination,
        (&raw mut out).cast(),
    );
    let copy = out as *const AsterArray;
    unsafe {
        assert_eq!((*copy).length, length);
        assert_eq!(
            (*copy).data.add(199_999 * 4).cast::<i32>().read_unaligned(),
            199_999 * 3
        );
    }
}
