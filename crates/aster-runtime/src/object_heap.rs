//! MEM-EXP-1 experiment (stages B and C): a size-class heap for escaping class
//! objects and the reference-count primitives that will release into it.
//!
//! Not part of default execution: compiled only with the `mem-exp-heap`
//! feature. Fresh blocks come from the context's Persistent arena, so their
//! addresses are stable and every page is still released with the context;
//! released blocks return to a per-size-class free list and are zeroed again
//! when reused. Debug builds poison a released block so a use after release
//! reads an unmistakable pattern instead of plausible data.
//!
//! Object layout used by the primitives (H12, the stage A candidate):
//! offset 0 holds a pointer to an immutable [`ObjectDescriptor`]; offset 8
//! holds the strong count in the low 29 bits (three high bits are reserved
//! flags). [`IMMORTAL`] marks objects that are never counted: region-owned,
//! module-owned, or saturated.

#![allow(
    clippy::cast_ptr_alignment,
    reason = "objects are 8-byte aligned by the allocator contract"
)]

use std::ptr;

/// Block sizes, in bytes. A larger object is not managed by the heap.
pub(crate) const SIZE_CLASSES: [u32; 15] = [
    16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048,
];

/// Offset of the count word in a counted object.
pub const COUNT_OFFSET: usize = 8;
/// Mask of the strong count inside the count word.
pub const COUNT_MASK: u32 = (1 << 29) - 1;
/// Count value meaning "never counted, never released".
pub const IMMORTAL: u32 = COUNT_MASK;
/// Kind flag in the count word's reserved high bits: the object is an array
/// header whose descriptor word names the element descriptor (MEM-EXP-1 U8).
pub const ARRAY_FLAG: u32 = 1 << 29;

#[cfg(debug_assertions)]
const POISON: u8 = 0xDB;

/// Immutable per-class metadata the descriptor word points to.
#[repr(C)]
pub struct ObjectDescriptor {
    /// Allocation size of the object, header included.
    pub size: u32,
    /// Number of entries in `reference_offsets`.
    pub reference_count: u32,
    /// Byte offsets of the fields that hold strong class references.
    pub reference_offsets: *const u32,
}

// SAFETY: a descriptor and its offset table are immutable after creation
// (module data, or `static` in tests) and are only ever read, so sharing a
// reference across threads cannot race.
#[allow(unsafe_code)]
unsafe impl Sync for ObjectDescriptor {}

/// Counters for the experiment report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjectHeapStats {
    pub fresh_blocks: u64,
    pub reused_blocks: u64,
    pub released_blocks: u64,
    /// Retains that changed a count (non-null, non-immortal operand).
    #[cfg(feature = "mem-exp-rc")]
    pub retains: u64,
    /// Releases that dropped a count (non-null, non-immortal operand).
    #[cfg(feature = "mem-exp-rc")]
    pub releases: u64,
    /// Arrays whose header (and heap-served data block) were released.
    #[cfg(feature = "mem-exp-rc")]
    pub released_arrays: u64,
    /// Array data blocks above the largest size class: never returned to the
    /// heap, they stay reserved until the context drops.
    #[cfg(feature = "mem-exp-rc")]
    pub unreturned_array_blocks: u64,
}

#[derive(Default)]
pub(crate) struct ObjectHeap {
    free: [Vec<*mut u8>; SIZE_CLASSES.len()],
    pub(crate) stats: ObjectHeapStats,
}

impl ObjectHeap {
    /// Index of the smallest size class that holds `size` bytes.
    pub(crate) fn class_of(size: u32) -> Option<usize> {
        SIZE_CLASSES.iter().position(|&class| class >= size)
    }

    /// A zeroed released block of `class`, if one is available.
    pub(crate) fn take_free(&mut self, class: usize) -> Option<*mut u8> {
        let block = self.free[class].pop()?;
        // SAFETY: `block` was handed out by this heap for exactly
        // `SIZE_CLASSES[class]` bytes of arena storage that stays mapped until
        // the owning context drops, and nothing references it while it is on
        // the free list.
        #[allow(unsafe_code)]
        unsafe {
            ptr::write_bytes(block, 0, SIZE_CLASSES[class] as usize);
        }
        self.stats.reused_blocks += 1;
        Some(block)
    }

    /// Return a block of `class` to the heap. The caller guarantees that no
    /// reference to it remains.
    pub(crate) fn release(&mut self, block: *mut u8, class: usize) {
        #[cfg(debug_assertions)]
        // SAFETY: as in `take_free`; the block is owned by this heap again.
        #[allow(unsafe_code)]
        unsafe {
            ptr::write_bytes(block, POISON, SIZE_CLASSES[class] as usize);
        }
        self.stats.released_blocks += 1;
        self.free[class].push(block);
    }
}

fn count_word(object: *mut u8) -> *mut u32 {
    object.wrapping_add(COUNT_OFFSET).cast::<u32>()
}

/// Read an object's strong count.
///
/// # Safety
///
/// `object` must point to a live counted object with the H12 layout.
#[allow(unsafe_code)]
pub unsafe fn strong_count(object: *mut u8) -> u32 {
    // SAFETY: the caller's contract.
    unsafe { count_word(object).read() & COUNT_MASK }
}

/// Write an initial count (1 for a fresh counted object, [`IMMORTAL`] for an
/// uncounted one).
///
/// # Safety
///
/// `object` must point to a live object with the H12 layout.
#[allow(unsafe_code)]
pub unsafe fn set_count(object: *mut u8, count: u32) {
    // SAFETY: the caller's contract.
    unsafe {
        let word = count_word(object);
        word.write((word.read() & !COUNT_MASK) | (count & COUNT_MASK));
    }
}

/// Add one strong reference. Saturates to [`IMMORTAL`]: an object referenced
/// more than 2^29 - 2 times is never released, which leaks and never frees
/// early.
///
/// # Safety
///
/// `object` must point to a live counted or immortal object.
#[allow(unsafe_code)]
pub unsafe fn retain(object: *mut u8) {
    // SAFETY: the caller's contract.
    let count = unsafe { strong_count(object) };
    if count == IMMORTAL {
        return;
    }
    // SAFETY: the caller's contract.
    unsafe { set_count(object, (count + 1).min(IMMORTAL)) };
}

/// Drop one strong reference and release every object whose count reaches
/// zero, with an explicit work list (no native recursion), releasing each
/// object's reference fields before returning its block to `heap`.
///
/// Returns the number of objects released.
///
/// # Safety
///
/// `object` and every object reachable from it through descriptor reference
/// offsets must be live counted or immortal objects whose descriptor word
/// points to a valid [`ObjectDescriptor`], and whose blocks were allocated by
/// `heap` when they are counted.
#[allow(unsafe_code)]
pub(crate) unsafe fn release(heap: &mut ObjectHeap, object: *mut u8) -> u64 {
    let mut work = vec![object];
    let mut released = 0;
    while let Some(current) = work.pop() {
        if current.is_null() {
            continue;
        }
        // SAFETY: the caller's contract covers every reachable object.
        let count = unsafe { strong_count(current) };
        if count == IMMORTAL {
            continue;
        }
        debug_assert!(
            count > 0,
            "release of an object whose count is already zero"
        );
        if count > 1 {
            // SAFETY: as above.
            unsafe { set_count(current, count - 1) };
            continue;
        }
        #[cfg(feature = "mem-exp-rc")]
        // SAFETY: the count word carries the kind flag by the layout contract.
        if unsafe { count_word(current).read() } & ARRAY_FLAG != 0 {
            // SAFETY: a flagged object is an array header of this heap.
            unsafe { release_array(heap, current, &mut work) };
            released += 1;
            continue;
        }
        // SAFETY: offset 0 holds the descriptor pointer by the layout
        // contract, and the descriptor and its offset table are immutable.
        let descriptor = unsafe { &*current.cast::<*const ObjectDescriptor>().read() };
        for index in 0..descriptor.reference_count as usize {
            // SAFETY: each offset names an in-bounds, pointer-aligned field.
            let child = unsafe {
                let offset = descriptor.reference_offsets.add(index).read() as usize;
                current.add(offset).cast::<*mut u8>().read()
            };
            work.push(child);
        }
        let class = ObjectHeap::class_of(descriptor.size)
            .expect("counted objects are allocated from a size class");
        heap.release(current, class);
        released += 1;
    }
    released
}

/// Free a counted array whose count reached zero: push every leaf of every
/// element, return the data block (when heap-served) and the header block.
///
/// # Safety
///
/// `array` must be a counted array header allocated by `heap` whose element
/// descriptor is null or valid and whose storage holds `length` live elements.
#[cfg(feature = "mem-exp-rc")]
#[allow(unsafe_code)]
unsafe fn release_array(heap: &mut ObjectHeap, array: *mut u8, work: &mut Vec<*mut u8>) {
    use crate::context::AsterArray;
    // SAFETY: the caller's contract.
    let (descriptor, length, element_size, data) = unsafe {
        let header = &*array.cast::<AsterArray>();
        (
            header.element_descriptor(),
            usize::try_from(header.length()).unwrap_or(0),
            header.element_size() as usize,
            header.data_ptr(),
        )
    };
    if !descriptor.is_null() && !data.is_null() {
        // SAFETY: the descriptor is immutable program data and every offset
        // lies inside one of the `length` elements.
        unsafe {
            let descriptor = &*descriptor;
            for element in 0..length {
                let base = data.add(element * element_size);
                for index in 0..descriptor.reference_count as usize {
                    let offset = descriptor.reference_offsets.add(index).read() as usize;
                    work.push(base.add(offset).cast::<*mut u8>().read_unaligned());
                }
            }
        }
    }
    let bytes = length.saturating_mul(element_size).max(1);
    match u32::try_from(bytes).ok().and_then(ObjectHeap::class_of) {
        Some(class) => heap.release(data, class),
        None => heap.stats.unreturned_array_blocks += 1,
    }
    let header_class = ObjectHeap::class_of(u32::try_from(size_of::<AsterArray>()).unwrap_or(0))
        .expect("array headers fit a size class");
    heap.release(array, header_class);
    heap.stats.released_arrays += 1;
}
