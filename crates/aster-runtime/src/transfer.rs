//! Domain-independent description of a value that crosses a task boundary by
//! deep copy (CONC-005, master plan section 8.7 and invariant H4: no
//! reference crosses a domain, transfers copy).
//!
//! A [`TransferShape`] describes the *type* of the value in terms of memory
//! images only (scalar widths, pointers to runtime-owned strings, arrays,
//! lists and dictionaries, struct and enum field offsets). It carries no
//! compiler identity, so the runtime can serialize a value in the source
//! `ExecutionContext` into a [`TransferWire`] (plain bytes, no pointer into
//! the source context) and materialize it again in the destination context
//! with fresh allocations (see `ExecutionContext::transfer_pack` and
//! `ExecutionContext::transfer_unpack`).
//!
//! The shape is produced by the compiler backend and handed to the runtime as
//! a compact byte string ([`TransferShape::encode`] / [`TransferShape::decode`])
//! so generated code can pass it as one constant pointer.
//!
//! # Wire format
//!
//! Everything is little-endian and written depth first, following the shape:
//!
//! * `Scalar { size }`: the `size` raw bytes of the value.
//! * `String`: one presence byte (`0` null, `1` present); if present a `u32`
//!   byte length and the UTF-8 payload.
//! * `Array`: presence byte; if present a `u32` length and every element.
//! * `List`: presence byte; if present `u32` length, `u32` element alignment,
//!   `u64` element type key and every element.
//! * `Dictionary`: presence byte; if present `u8` key kind, `u32` key
//!   alignment, `u32` value alignment, `u64` key type key, `u64` value type
//!   key, `u32` live entry count and every `(key, value)` in insertion order.
//! * `Struct`: every field in shape order (padding is not transferred).
//! * `Enum`: the `u32` tag, then the fields of that case.
//!
//! Aliasing is not preserved: a value reachable twice is copied twice.

/// Upper bound on the nesting depth of a decoded shape. Real shapes are as
/// deep as the (finite, non-recursive) type, so this only rejects corrupt or
/// hostile encodings before they can exhaust the native stack.
pub const MAX_TRANSFER_SHAPE_DEPTH: usize = 64;

const TAG_SCALAR: u8 = 0;
const TAG_STRING: u8 = 1;
const TAG_ARRAY: u8 = 2;
const TAG_LIST: u8 = 3;
const TAG_DICTIONARY: u8 = 4;
const TAG_STRUCT: u8 = 5;
const TAG_ENUM: u8 = 6;

/// One enum case of a [`TransferShape::Enum`]: its tag and the fields that
/// are live when the tag is set (offsets are from the start of the enum value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferCase {
    pub tag: u32,
    pub fields: Vec<(u32, TransferShape)>,
}

/// Memory-image description of one transferable type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferShape {
    /// A fixed-width scalar of `size` bytes (1, 2, 4 or 8).
    Scalar { size: u32 },
    /// A pointer to an immutable runtime string.
    String,
    /// A pointer to an array header; elements are laid out with the stride of
    /// the element image. `descriptor` is the element descriptor address of
    /// the counting configuration (`0` when elements have no reference leaves
    /// or counting is not compiled in).
    Array {
        element: Box<TransferShape>,
        descriptor: u64,
    },
    /// A pointer to a `List<T>` header; `descriptor` as for `Array`.
    List {
        element: Box<TransferShape>,
        descriptor: u64,
    },
    /// A pointer to a `Dictionary<K, V>` header; `descriptors` is the address
    /// of the `{ key, value }` descriptor pair of the counting configuration
    /// (`0` when neither side has reference leaves).
    Dictionary {
        key: Box<TransferShape>,
        value: Box<TransferShape>,
        descriptors: u64,
    },
    /// An inline struct of `size` bytes.
    Struct {
        size: u32,
        fields: Vec<(u32, TransferShape)>,
    },
    /// An inline tagged enum of `size` bytes; the tag is a `u32` at offset 0.
    Enum { size: u32, cases: Vec<TransferCase> },
}

impl TransferShape {
    /// Size in bytes of the memory image of one value of this shape: a
    /// pointer for strings and collections, the scalar width, or the inline
    /// size of a struct/enum.
    #[must_use]
    pub fn image_size(&self) -> usize {
        match self {
            Self::String | Self::Array { .. } | Self::List { .. } | Self::Dictionary { .. } => {
                size_of::<*const u8>()
            }
            Self::Scalar { size } | Self::Struct { size, .. } | Self::Enum { size, .. } => {
                *size as usize
            }
        }
    }

    /// Whether the value image holds anything beyond plain bytes (a pointer
    /// to runtime-owned memory somewhere inside it).
    #[must_use]
    pub fn has_references(&self) -> bool {
        match self {
            Self::Scalar { .. } => false,
            Self::String | Self::Array { .. } | Self::List { .. } | Self::Dictionary { .. } => true,
            Self::Struct { fields, .. } => fields.iter().any(|(_, field)| field.has_references()),
            Self::Enum { cases, .. } => cases
                .iter()
                .any(|case| case.fields.iter().any(|(_, field)| field.has_references())),
        }
    }

    /// Compact byte encoding for the generated-code ABI.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Scalar { size } => {
                out.push(TAG_SCALAR);
                out.extend_from_slice(&size.to_le_bytes());
            }
            Self::String => out.push(TAG_STRING),
            Self::Array {
                element,
                descriptor,
            } => {
                out.push(TAG_ARRAY);
                out.extend_from_slice(&descriptor.to_le_bytes());
                element.encode_into(out);
            }
            Self::List {
                element,
                descriptor,
            } => {
                out.push(TAG_LIST);
                out.extend_from_slice(&descriptor.to_le_bytes());
                element.encode_into(out);
            }
            Self::Dictionary {
                key,
                value,
                descriptors,
            } => {
                out.push(TAG_DICTIONARY);
                out.extend_from_slice(&descriptors.to_le_bytes());
                key.encode_into(out);
                value.encode_into(out);
            }
            Self::Struct { size, fields } => {
                out.push(TAG_STRUCT);
                out.extend_from_slice(&size.to_le_bytes());
                push_count(out, fields.len());
                for (offset, field) in fields {
                    out.extend_from_slice(&offset.to_le_bytes());
                    field.encode_into(out);
                }
            }
            Self::Enum { size, cases } => {
                out.push(TAG_ENUM);
                out.extend_from_slice(&size.to_le_bytes());
                push_count(out, cases.len());
                for case in cases {
                    out.extend_from_slice(&case.tag.to_le_bytes());
                    push_count(out, case.fields.len());
                    for (offset, field) in &case.fields {
                        out.extend_from_slice(&offset.to_le_bytes());
                        field.encode_into(out);
                    }
                }
            }
        }
    }

    /// Decode and validate a shape produced by [`Self::encode`].
    ///
    /// # Errors
    ///
    /// Returns a description of the first malformed part: truncation, an
    /// unknown tag, a scalar width other than 1/2/4/8, a field that does not
    /// fit its struct, excessive depth, or trailing bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut cursor = ShapeCursor { bytes, position: 0 };
        let shape = cursor.shape(0)?;
        if cursor.position != bytes.len() {
            return Err("transfer shape has trailing bytes".to_owned());
        }
        Ok(shape)
    }
}

fn push_count(out: &mut Vec<u8>, count: usize) {
    // Struct field and enum case counts are bounded by the program's types.
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_le_bytes());
}

struct ShapeCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl ShapeCursor<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], String> {
        let end = self
            .position
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| "transfer shape is truncated".to_owned())?;
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes = self.take(8)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(array))
    }

    fn fields(&mut self, size: u32, depth: usize) -> Result<Vec<(u32, TransferShape)>, String> {
        let count = self.u32()?;
        let mut fields = Vec::new();
        for _ in 0..count {
            let offset = self.u32()?;
            let field = self.shape(depth + 1)?;
            let end = u64::from(offset) + field.image_size() as u64;
            if end > u64::from(size) {
                return Err("transfer shape field lies outside its aggregate".to_owned());
            }
            fields.push((offset, field));
        }
        Ok(fields)
    }

    fn shape(&mut self, depth: usize) -> Result<TransferShape, String> {
        if depth > MAX_TRANSFER_SHAPE_DEPTH {
            return Err("transfer shape is nested too deeply".to_owned());
        }
        Ok(match self.u8()? {
            TAG_SCALAR => {
                let size = self.u32()?;
                if !matches!(size, 1 | 2 | 4 | 8) {
                    return Err(format!("transfer shape has invalid scalar width {size}"));
                }
                TransferShape::Scalar { size }
            }
            TAG_STRING => TransferShape::String,
            TAG_ARRAY => {
                let descriptor = self.u64()?;
                TransferShape::Array {
                    element: Box::new(self.shape(depth + 1)?),
                    descriptor,
                }
            }
            TAG_LIST => {
                let descriptor = self.u64()?;
                TransferShape::List {
                    element: Box::new(self.shape(depth + 1)?),
                    descriptor,
                }
            }
            TAG_DICTIONARY => {
                let descriptors = self.u64()?;
                let key = Box::new(self.shape(depth + 1)?);
                let value = Box::new(self.shape(depth + 1)?);
                TransferShape::Dictionary {
                    key,
                    value,
                    descriptors,
                }
            }
            TAG_STRUCT => {
                let size = self.u32()?;
                let fields = self.fields(size, depth)?;
                TransferShape::Struct { size, fields }
            }
            TAG_ENUM => {
                let size = self.u32()?;
                let count = self.u32()?;
                let mut cases = Vec::new();
                for _ in 0..count {
                    let tag = self.u32()?;
                    let fields = self.fields(size, depth)?;
                    cases.push(TransferCase { tag, fields });
                }
                if size < 4 {
                    return Err("transfer shape enum is smaller than its tag".to_owned());
                }
                TransferShape::Enum { size, cases }
            }
            other => return Err(format!("transfer shape has unknown tag {other}")),
        })
    }
}

/// The serialized bytes of one transferred value (see the module-level wire
/// format). Plain owned data: `Send`, `Clone`, and free of any pointer into
/// the context it was packed from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferWire(Vec<u8>);

impl TransferWire {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One argument of a `Task.Run` call that crosses by deep copy: where its
/// image goes in the argument frame, its encoded shape and its serialized
/// bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferItem {
    pub frame_offset: u32,
    pub shape: Vec<u8>,
    pub wire: TransferWire,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TransferShape {
        TransferShape::Dictionary {
            key: Box::new(TransferShape::String),
            value: Box::new(TransferShape::Array {
                element: Box::new(TransferShape::Struct {
                    size: 16,
                    fields: vec![
                        (0, TransferShape::Scalar { size: 4 }),
                        (8, TransferShape::String),
                    ],
                }),
                descriptor: 0xABCD,
            }),
            descriptors: 7,
        }
    }

    #[test]
    fn shape_round_trips_through_its_encoding() {
        let shape = sample();
        assert_eq!(TransferShape::decode(&shape.encode()), Ok(shape));
    }

    #[test]
    fn enum_shape_round_trips() {
        let shape = TransferShape::Enum {
            size: 16,
            cases: vec![
                TransferCase {
                    tag: 0,
                    fields: vec![(8, TransferShape::String)],
                },
                TransferCase {
                    tag: 5,
                    fields: Vec::new(),
                },
            ],
        };
        assert_eq!(TransferShape::decode(&shape.encode()), Ok(shape));
    }

    #[test]
    fn decode_rejects_truncation_trailing_bytes_and_bad_tags() {
        let encoded = sample().encode();
        for cut in 0..encoded.len() {
            assert!(
                TransferShape::decode(&encoded[..cut]).is_err(),
                "cut at {cut}"
            );
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(TransferShape::decode(&trailing).is_err());
        assert!(TransferShape::decode(&[99]).is_err());
    }

    #[test]
    fn decode_rejects_bad_scalar_widths_misplaced_fields_and_deep_nesting() {
        let mut bad_width = vec![TAG_SCALAR];
        bad_width.extend_from_slice(&3_u32.to_le_bytes());
        assert!(TransferShape::decode(&bad_width).is_err());

        let outside = TransferShape::Struct {
            size: 4,
            fields: vec![(2, TransferShape::Scalar { size: 4 })],
        };
        assert!(TransferShape::decode(&outside.encode()).is_err());

        let mut deep = TransferShape::String;
        for _ in 0..=MAX_TRANSFER_SHAPE_DEPTH + 1 {
            deep = TransferShape::Array {
                element: Box::new(deep),
                descriptor: 0,
            };
        }
        assert!(TransferShape::decode(&deep.encode()).is_err());
    }

    #[test]
    fn image_size_and_reference_queries() {
        assert_eq!(TransferShape::Scalar { size: 2 }.image_size(), 2);
        assert_eq!(TransferShape::String.image_size(), size_of::<*const u8>());
        assert!(!TransferShape::Scalar { size: 8 }.has_references());
        assert!(sample().has_references());
        let plain = TransferShape::Struct {
            size: 8,
            fields: vec![(0, TransferShape::Scalar { size: 8 })],
        };
        assert!(!plain.has_references());
    }
}
