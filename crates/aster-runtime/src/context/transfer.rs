//! Serialization and materialization of deep-copy transfers (CONC-005).
//!
//! A child module of `context` so it can read the private runtime headers.
//! `transfer_pack` reads a value in this (source) context into a
//! [`TransferWire`] holding no pointer into the context; `transfer_unpack`
//! rebuilds it in this (destination) context through the ordinary allocation
//! paths, so every string, array, list and dictionary is a fresh allocation
//! owned by the destination (reference counts initialize as for any new
//! value; the destination root keeps its initial count, which is the
//! documented per-context "transfer root" retention).

#![allow(clippy::wildcard_imports, clippy::missing_errors_doc)]

use super::*;
use crate::transfer::{TransferItem, TransferShape, TransferWire};

const MAX_WIRE_BYTES: usize = EXECUTION_ALLOCATION_LIMIT_BYTES;

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn put(&mut self, data: &[u8]) -> Result<(), String> {
        let total = self.bytes.len().checked_add(data.len());
        if total.is_none_or(|total| total > MAX_WIRE_BYTES) {
            return Err("transferred value exceeds the transfer size limit".to_owned());
        }
        self.bytes
            .try_reserve(data.len())
            .map_err(|_| "transfer buffer allocation failed".to_owned())?;
        self.bytes.extend_from_slice(data);
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| "transfer data is truncated".to_owned())?;
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let mut a = [0_u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(a))
    }
}

fn to_u32(value: usize) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| "transferred collection is too large".to_owned())
}

impl ExecutionContext {
    /// Whether a controlled runtime error has been recorded.
    #[must_use]
    pub fn has_failed(&self) -> bool {
        self.error.is_some()
    }

    /// Queue one packed argument for the `Task.Run` call being assembled;
    /// `index == 0` starts a fresh set.
    pub fn push_pending_transfer(&mut self, index: i32, item: TransferItem) {
        if index == 0 {
            self.pending_transfers.clear();
        }
        self.pending_transfers.push(item);
    }

    /// Take (and clear) the queued transfers.
    pub fn take_pending_transfers(&mut self) -> Vec<TransferItem> {
        std::mem::take(&mut self.pending_transfers)
    }

    /// Serialize the value whose memory image is at `value`.
    ///
    /// # Safety
    ///
    /// `value` must be readable for `shape.image_size()` bytes and hold a
    /// value of exactly that shape, whose references were produced by this
    /// context (or are module data).
    #[allow(unsafe_code)]
    pub unsafe fn transfer_pack(
        &mut self,
        shape: &TransferShape,
        value: *const u8,
    ) -> Result<TransferWire, String> {
        let mut writer = Writer { bytes: Vec::new() };
        // SAFETY: forwarded caller contract.
        unsafe { self.pack_value(shape, value, &mut writer)? };
        Ok(TransferWire::new(writer.bytes))
    }

    /// Materialize `wire` into fresh allocations of this context and write the
    /// value image at `destination` (zeroed first).
    ///
    /// # Safety
    ///
    /// `destination` must be writable for `shape.image_size()` bytes.
    #[allow(unsafe_code)]
    pub unsafe fn transfer_unpack(
        &mut self,
        shape: &TransferShape,
        wire: &[u8],
        destination: *mut u8,
    ) -> Result<(), String> {
        let mut reader = Reader {
            bytes: wire,
            position: 0,
        };
        // SAFETY: forwarded caller contract.
        unsafe {
            ptr::write_bytes(destination, 0, shape.image_size());
            self.unpack_value(shape, &mut reader, destination)?;
        }
        if reader.position != wire.len() {
            return Err("transfer data has trailing bytes".to_owned());
        }
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(())
    }

    /// Record the element descriptor of a freshly built array (counting
    /// configuration only; a no-op otherwise).
    ///
    /// # Safety
    ///
    /// `array` must be a live array header of this context.
    #[allow(unsafe_code, unused_variables)]
    pub unsafe fn transfer_set_array_descriptor(
        &mut self,
        array: *mut AsterArray,
        descriptor: u64,
    ) {
        #[cfg(feature = "mem-exp-rc")]
        if !array.is_null() && descriptor != 0 {
            // SAFETY: caller contract; the descriptor is immutable program data.
            unsafe {
                (*array).descriptor = descriptor as *const crate::object_heap::ObjectDescriptor;
            }
        }
    }

    #[allow(unsafe_code, clippy::too_many_lines)]
    unsafe fn pack_value(
        &mut self,
        shape: &TransferShape,
        image: *const u8,
        out: &mut Writer,
    ) -> Result<(), String> {
        match shape {
            TransferShape::Scalar { size } => {
                // SAFETY: caller contract: `image` holds `size` readable bytes.
                let bytes = unsafe { std::slice::from_raw_parts(image, *size as usize) };
                out.put(bytes)
            }
            TransferShape::String => {
                // SAFETY: the image holds a string pointer.
                let string = unsafe { ptr::read_unaligned(image.cast::<*const AsterStrHeader>()) };
                if string.is_null() {
                    return out.put(&[0]);
                }
                // SAFETY: non-null strings are runtime or module data in the ABI layout.
                let Some(text) = (unsafe { crate::string::view(string) }) else {
                    return Err("cannot transfer an invalid string".to_owned());
                };
                out.put(&[1])?;
                out.put(&to_u32(text.len())?.to_le_bytes())?;
                out.put(text.as_bytes())
            }
            TransferShape::Array { element, .. } => {
                // SAFETY: the image holds an array pointer.
                let array = unsafe { ptr::read_unaligned(image.cast::<*const AsterArray>()) };
                if array.is_null() {
                    return out.put(&[0]);
                }
                // SAFETY: array headers are runtime-owned.
                let (length, element_size, data) =
                    unsafe { ((*array).length, (*array).element_size, (*array).data) };
                let stride = element.image_size();
                if length < 0 || element_size as usize != stride || (length > 0 && data.is_null()) {
                    return Err("array header does not match its transfer shape".to_owned());
                }
                let length = usize::try_from(length).unwrap_or(0);
                out.put(&[1])?;
                out.put(&to_u32(length)?.to_le_bytes())?;
                for index in 0..length {
                    // SAFETY: `index < length` elements of `stride` bytes live at `data`.
                    unsafe { self.pack_value(element, data.add(index * stride), out)? };
                }
                Ok(())
            }
            TransferShape::List { element, .. } => {
                // SAFETY: the image holds a list pointer.
                let list = unsafe { ptr::read_unaligned(image.cast::<*const AsterList>()) };
                if list.is_null() {
                    return out.put(&[0]);
                }
                if !self.validate_list_header(list) {
                    return Err("list header is invalid".to_owned());
                }
                // SAFETY: header validated above.
                let (length, size, align, key, data) = unsafe {
                    (
                        (*list).length,
                        (*list).element_size,
                        (*list).element_align,
                        (*list).element_type_key,
                        (*list).data,
                    )
                };
                let stride = element.image_size();
                if size as usize != stride {
                    return Err("list header does not match its transfer shape".to_owned());
                }
                let length = usize::try_from(length).unwrap_or(0);
                out.put(&[1])?;
                out.put(&to_u32(length)?.to_le_bytes())?;
                out.put(&align.to_le_bytes())?;
                out.put(&key.to_le_bytes())?;
                for index in 0..length {
                    // SAFETY: validated `length <= capacity` elements live at `data`.
                    unsafe { self.pack_value(element, data.add(index * stride), out)? };
                }
                Ok(())
            }
            TransferShape::Dictionary { key, value, .. } => {
                // SAFETY: the image holds a dictionary pointer.
                let dictionary =
                    unsafe { ptr::read_unaligned(image.cast::<*const AsterDictionary>()) };
                if dictionary.is_null() {
                    return out.put(&[0]);
                }
                if !self.validate_dictionary_header(dictionary) {
                    return Err("dictionary header is invalid".to_owned());
                }
                // SAFETY: header validated above.
                let (kind, ks, ka, vs, va, ktk, vtk, length, count, entries) = unsafe {
                    (
                        (*dictionary).key_kind,
                        (*dictionary).key_size,
                        (*dictionary).key_align,
                        (*dictionary).value_size,
                        (*dictionary).value_align,
                        (*dictionary).key_type_key,
                        (*dictionary).value_type_key,
                        (*dictionary).length,
                        (*dictionary).entry_count,
                        (*dictionary).entries,
                    )
                };
                if ks as usize != key.image_size() || vs as usize != value.image_size() {
                    return Err("dictionary header does not match its transfer shape".to_owned());
                }
                let Some(layout) = dictionary_entry_layout(ks, ka, vs, va) else {
                    return Err("dictionary header has an invalid layout".to_owned());
                };
                out.put(&[1, kind as u8])?;
                out.put(&ka.to_le_bytes())?;
                out.put(&va.to_le_bytes())?;
                out.put(&ktk.to_le_bytes())?;
                out.put(&vtk.to_le_bytes())?;
                out.put(&to_u32(usize::try_from(length).unwrap_or(0))?.to_le_bytes())?;
                let mut live = 0;
                for index in 0..count {
                    let Some(entry) = Self::dictionary_entry_pointer(entries, index, layout) else {
                        return Err("dictionary entry offset overflow".to_owned());
                    };
                    // SAFETY: `index < entry_count <= entry_capacity`.
                    if unsafe { Self::dictionary_entry_live(entry) } != 1 {
                        continue;
                    }
                    live += 1;
                    // SAFETY: live entries hold a key and value at their offsets.
                    unsafe {
                        self.pack_value(key, entry.add(layout.key_offset), out)?;
                        self.pack_value(value, entry.add(layout.value_offset), out)?;
                    }
                }
                if live != length {
                    return Err("dictionary live entry count is inconsistent".to_owned());
                }
                Ok(())
            }
            TransferShape::Struct { fields, .. } => {
                for (offset, field) in fields {
                    // SAFETY: field offsets were validated against the struct size.
                    unsafe { self.pack_value(field, image.add(*offset as usize), out)? };
                }
                Ok(())
            }
            TransferShape::Enum { cases, .. } => {
                // SAFETY: enum images start with a `u32` tag.
                let tag = unsafe { ptr::read_unaligned(image.cast::<u32>()) };
                let Some(case) = cases.iter().find(|case| case.tag == tag) else {
                    return Err(format!("enum tag {tag} is not part of its transfer shape"));
                };
                out.put(&tag.to_le_bytes())?;
                for (offset, field) in &case.fields {
                    // SAFETY: field offsets were validated against the enum size.
                    unsafe { self.pack_value(field, image.add(*offset as usize), out)? };
                }
                Ok(())
            }
        }
    }

    #[allow(unsafe_code, clippy::too_many_lines)]
    unsafe fn unpack_value(
        &mut self,
        shape: &TransferShape,
        input: &mut Reader<'_>,
        dest: *mut u8,
    ) -> Result<(), String> {
        match shape {
            TransferShape::Scalar { size } => {
                let bytes = input.take(*size as usize)?;
                // SAFETY: caller contract: `dest` is writable for the image.
                unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), dest, bytes.len()) };
                Ok(())
            }
            TransferShape::String => {
                if input.u8()? == 0 {
                    return Ok(());
                }
                let length = input.u32()? as usize;
                let bytes = input.take(length)?;
                let string = self.allocate_string_storage(length, false);
                if string.is_null() {
                    return Err(
                        "string allocation failed while materializing a transfer".to_owned()
                    );
                }
                // SAFETY: the string owns `length` payload bytes; `dest` holds a pointer slot.
                unsafe {
                    ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        string.cast::<u8>().add(size_of::<AsterStrHeader>()),
                        length,
                    );
                    ptr::write_unaligned(dest.cast::<*const AsterStrHeader>(), string);
                }
                Ok(())
            }
            TransferShape::Array {
                element,
                descriptor,
            } => {
                if input.u8()? == 0 {
                    return Ok(());
                }
                let length = i32::try_from(input.u32()?)
                    .map_err(|_| "transferred array is too large".to_owned())?;
                let stride = to_u32(element.image_size())?;
                let array = self.allocate_array(length, stride);
                if array.is_null() {
                    return Err("array allocation failed while materializing a transfer".to_owned());
                }
                #[cfg(feature = "mem-exp-rc")]
                {
                    // SAFETY: freshly allocated header.
                    unsafe { self.transfer_set_array_descriptor(array, *descriptor) };
                    if *descriptor == 0 && matches!(**element, TransferShape::String) {
                        Self::set_string_array_descriptor(array);
                    }
                }
                #[cfg(not(feature = "mem-exp-rc"))]
                let _ = descriptor;
                // SAFETY: fresh zeroed storage for `length` elements.
                let data = unsafe { (*array).data };
                for index in 0..usize::try_from(length).unwrap_or(0) {
                    // SAFETY: element `index` lies inside the fresh data block.
                    unsafe {
                        self.unpack_value(element, input, data.add(index * stride as usize))?;
                    }
                }
                // SAFETY: `dest` holds a pointer slot.
                unsafe { ptr::write_unaligned(dest.cast::<*mut AsterArray>(), array) };
                Ok(())
            }
            TransferShape::List {
                element,
                descriptor,
            } => {
                if input.u8()? == 0 {
                    return Ok(());
                }
                let length = input.u32()?;
                let align = input.u32()?;
                let key = input.u64()?;
                let size = to_u32(element.image_size())?;
                let list = self.allocate_list_in_region(size, align, key, ListRegion::Persistent);
                if list.is_null() {
                    return Err("list allocation failed while materializing a transfer".to_owned());
                }
                #[cfg(feature = "mem-exp-rc")]
                if *descriptor != 0 {
                    // SAFETY: fresh header; descriptor is immutable program data.
                    unsafe {
                        (*list).element_descriptor =
                            *descriptor as *const crate::object_heap::ObjectDescriptor;
                    }
                } else if matches!(**element, TransferShape::String) {
                    // SAFETY: fresh header; static runtime descriptor.
                    unsafe {
                        (*list).element_descriptor =
                            &raw const crate::object_heap::STRING_ELEMENT_DESCRIPTOR;
                    }
                }
                #[cfg(not(feature = "mem-exp-rc"))]
                let _ = descriptor;
                let mut buffer = vec![0_u64; (size as usize).div_ceil(8).max(1)];
                for _ in 0..length {
                    buffer.fill(0);
                    let slot = buffer.as_mut_ptr().cast::<u8>();
                    // SAFETY: `buffer` covers `size` bytes.
                    unsafe { self.unpack_value(element, input, slot)? };
                    self.list_add(list, size, align, key, slot);
                    if let Some(error) = &self.error {
                        return Err(error.clone());
                    }
                }
                // SAFETY: `dest` holds a pointer slot.
                unsafe { ptr::write_unaligned(dest.cast::<*mut AsterList>(), list) };
                Ok(())
            }
            TransferShape::Dictionary {
                key,
                value,
                descriptors,
            } => {
                if input.u8()? == 0 {
                    return Ok(());
                }
                let kind = DictionaryKeyKind::from_abi(i32::from(input.u8()?))
                    .ok_or_else(|| "transferred dictionary has an invalid key kind".to_owned())?;
                let ka = input.u32()?;
                let va = input.u32()?;
                let ktk = input.u64()?;
                let vtk = input.u64()?;
                let count = input.u32()?;
                let ks = to_u32(key.image_size())?;
                let vs = to_u32(value.image_size())?;
                let dictionary = self.allocate_dictionary_in_region(
                    kind,
                    ks,
                    ka,
                    ktk,
                    vs,
                    va,
                    vtk,
                    ListRegion::Persistent,
                );
                if dictionary.is_null() {
                    return Err(
                        "dictionary allocation failed while materializing a transfer".to_owned(),
                    );
                }
                #[cfg(feature = "mem-exp-rc")]
                if *descriptors != 0 {
                    // SAFETY: fresh header; descriptor pair is immutable program data.
                    unsafe {
                        (*dictionary).descriptors =
                            *descriptors as *const crate::object_heap::DictionaryDescriptors;
                    }
                }
                #[cfg(not(feature = "mem-exp-rc"))]
                let _ = descriptors;
                let mut key_buffer = vec![0_u64; (ks as usize).div_ceil(8).max(1)];
                let mut value_buffer = vec![0_u64; (vs as usize).div_ceil(8).max(1)];
                for _ in 0..count {
                    key_buffer.fill(0);
                    value_buffer.fill(0);
                    let key_slot = key_buffer.as_mut_ptr().cast::<u8>();
                    let value_slot = value_buffer.as_mut_ptr().cast::<u8>();
                    // SAFETY: the buffers cover the key and value images.
                    unsafe {
                        self.unpack_value(key, input, key_slot)?;
                        self.unpack_value(value, input, value_slot)?;
                    }
                    let inserted = self.dictionary_add_or_set(
                        dictionary, kind, ks, ka, ktk, vs, va, vtk, key_slot, value_slot, false,
                    );
                    if let Some(error) = &self.error {
                        return Err(error.clone());
                    }
                    if inserted != 1 {
                        return Err("transferred dictionary contains a duplicate key".to_owned());
                    }
                }
                // SAFETY: `dest` holds a pointer slot.
                unsafe { ptr::write_unaligned(dest.cast::<*mut AsterDictionary>(), dictionary) };
                Ok(())
            }
            TransferShape::Struct { fields, .. } => {
                for (offset, field) in fields {
                    // SAFETY: offsets were validated against the struct size.
                    unsafe { self.unpack_value(field, input, dest.add(*offset as usize))? };
                }
                Ok(())
            }
            TransferShape::Enum { cases, .. } => {
                let tag = input.u32()?;
                let Some(case) = cases.iter().find(|case| case.tag == tag) else {
                    return Err(format!("enum tag {tag} is not part of its transfer shape"));
                };
                // SAFETY: enum images start with a `u32` tag.
                unsafe { ptr::write_unaligned(dest.cast::<u32>(), tag) };
                for (offset, field) in &case.fields {
                    // SAFETY: offsets were validated against the enum size.
                    unsafe { self.unpack_value(field, input, dest.add(*offset as usize))? };
                }
                Ok(())
            }
        }
    }
}
