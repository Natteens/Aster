//! Paged bump allocator for per-execution memory.
//!
//! Each [`PagedArena`] owns a sequence of pages allocated via
//! [`std::alloc::alloc_zeroed`]. Allocations bump a cursor within the active
//! (last) page. When the active page is exhausted, a new one is appended.
//! Pages are never moved or freed individually — all memory is released when
//! the arena is dropped.
//!
//! Requests larger than [`DEFAULT_PAGE_SIZE`] get a dedicated page whose
//! capacity equals the request size.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};

/// Default page capacity in bytes.
const DEFAULT_PAGE_SIZE: usize = 64 * 1024;

/// Maximum alignment supported by arena allocations.
const MAX_ALIGN: usize = 16;

/// One contiguous page of zeroed memory.
struct Page {
    base: *mut u8,
    layout: Layout,
    cursor: usize,
}

impl Page {
    fn new(capacity: usize) -> Self {
        debug_assert!(capacity > 0);
        let layout = Layout::from_size_align(capacity, MAX_ALIGN)
            .expect("page layout exceeds platform limits");
        // SAFETY: `layout` has non-zero size and valid alignment. The returned
        // pointer owns `capacity` zeroed bytes.
        #[allow(unsafe_code)]
        let base = unsafe { alloc_zeroed(layout) };
        if base.is_null() {
            handle_alloc_error(layout);
        }
        Page {
            base,
            layout,
            cursor: 0,
        }
    }

    fn capacity(&self) -> usize {
        self.layout.size()
    }

    /// Try to place `size` bytes at the given alignment inside this page.
    /// Returns the pointer and the number of bytes consumed (including padding)
    /// on success.
    fn try_alloc(&mut self, size: usize, align: usize) -> Option<(*mut u8, usize)> {
        let aligned = align_up(self.cursor, align)?;
        let end = aligned.checked_add(size)?;
        if end > self.capacity() {
            return None;
        }
        let consumed = end - self.cursor;
        // SAFETY: `aligned` is within [0, capacity) and `base` points to a
        // live allocation of at least `capacity` bytes. The resulting pointer
        // stays within the page.
        #[allow(unsafe_code)]
        let ptr = unsafe { self.base.add(aligned) };
        self.cursor = end;
        Some((ptr, consumed))
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        // SAFETY: `self.base` was allocated with `alloc_zeroed(self.layout)` and
        // is freed exactly once here. No other code calls `dealloc` on it.
        #[allow(unsafe_code)]
        unsafe {
            dealloc(self.base, self.layout);
        }
    }
}

/// Cumulative allocation statistics from the arena.
pub(crate) struct ArenaMetrics {
    /// Total bytes consumed inside pages, including alignment padding.
    pub used_bytes: usize,
    /// Total capacity of all pages held by the arena.
    pub reserved_bytes: usize,
}

/// Paged bump allocator. Owns all pages and frees them on drop.
pub(crate) struct PagedArena {
    pages: Vec<Page>,
    used_bytes: usize,
    reserved_bytes: usize,
}

impl PagedArena {
    pub(crate) fn new() -> Self {
        PagedArena {
            pages: Vec::new(),
            used_bytes: 0,
            reserved_bytes: 0,
        }
    }

    /// Allocate `size` bytes with the given alignment. The returned pointer is
    /// stable for the arena's lifetime and points to zeroed memory.
    ///
    /// # Panics
    ///
    /// - If `size` is zero.
    /// - If `align` is not a non-zero power of two, or exceeds [`MAX_ALIGN`].
    /// - Via [`handle_alloc_error`] if the system allocator cannot
    ///   satisfy the underlying page allocation.
    pub(crate) fn alloc(&mut self, size: usize, align: usize) -> *mut u8 {
        assert!(size > 0, "zero-size allocation");
        assert!(align.is_power_of_two(), "alignment must be a non-zero power of two");
        assert!(align <= MAX_ALIGN, "alignment {align} exceeds MAX_ALIGN ({MAX_ALIGN})");

        if let Some(page) = self.pages.last_mut() {
            if let Some((ptr, consumed)) = page.try_alloc(size, align) {
                self.used_bytes += consumed;
                return ptr;
            }
        }

        // Fresh pages are MAX_ALIGN-aligned, so the first allocation needs no padding.
        let capacity = size.max(DEFAULT_PAGE_SIZE);

        let mut page = Page::new(capacity);
        let (ptr, consumed) = page
            .try_alloc(size, align)
            .expect("fresh page must satisfy the allocation");
        self.used_bytes += consumed;
        self.reserved_bytes += page.capacity();
        self.pages.push(page);
        ptr
    }

    pub(crate) fn metrics(&self) -> ArenaMetrics {
        ArenaMetrics {
            used_bytes: self.used_bytes,
            reserved_bytes: self.reserved_bytes,
        }
    }

    /// Number of pages currently held. Exposed for internal assertions only.
    #[cfg(test)]
    fn page_count(&self) -> usize {
        self.pages.len()
    }
}

impl Default for PagedArena {
    fn default() -> Self {
        Self::new()
    }
}

/// Round `value` up to the nearest multiple of `align`. Returns `None` on
/// overflow.
fn align_up(value: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    value.checked_add(mask).map(|v| v & !mask)
}

#[cfg(test)]
#[allow(clippy::cast_ptr_alignment, clippy::ptr_as_ptr)]
mod tests {
    use super::*;

    #[test]
    fn first_allocation_returns_aligned_non_null_pointer() {
        let mut arena = PagedArena::new();
        let ptr = arena.alloc(32, 8);
        assert!(!ptr.is_null());
        assert_eq!(ptr as usize % 8, 0);
        assert_eq!(arena.page_count(), 1);
    }

    #[test]
    fn multiple_small_allocations_share_one_page() {
        let mut arena = PagedArena::new();
        let ptrs: Vec<*mut u8> = (0..100).map(|_| arena.alloc(64, 8)).collect();
        assert_eq!(arena.page_count(), 1);
        for (i, &p) in ptrs.iter().enumerate() {
            for &q in &ptrs[i + 1..] {
                assert_ne!(p, q);
            }
        }
    }

    #[test]
    fn alignment_1() {
        let mut arena = PagedArena::new();
        for _ in 0..8 {
            let ptr = arena.alloc(1, 1);
            assert!(!ptr.is_null());
        }
    }

    #[test]
    fn alignment_2() {
        let mut arena = PagedArena::new();
        arena.alloc(1, 1);
        let ptr = arena.alloc(2, 2);
        assert_eq!(ptr as usize % 2, 0);
    }

    #[test]
    fn alignment_4() {
        let mut arena = PagedArena::new();
        arena.alloc(1, 1);
        let ptr = arena.alloc(4, 4);
        assert_eq!(ptr as usize % 4, 0);
    }

    #[test]
    fn alignment_8() {
        let mut arena = PagedArena::new();
        arena.alloc(3, 1);
        let ptr = arena.alloc(8, 8);
        assert_eq!(ptr as usize % 8, 0);
    }

    #[test]
    fn alignment_16() {
        let mut arena = PagedArena::new();
        arena.alloc(5, 1);
        let ptr = arena.alloc(16, 16);
        assert_eq!(ptr as usize % 16, 0);
    }

    #[test]
    fn padding_reflected_in_used_bytes() {
        let mut arena = PagedArena::new();
        arena.alloc(3, 1);
        arena.alloc(4, 8);
        let m = arena.metrics();
        assert_eq!(m.used_bytes, 12);
    }

    #[test]
    fn new_page_created_on_exhaustion() {
        let mut arena = PagedArena::new();
        for _ in 0..1024 {
            arena.alloc(64, 8);
        }
        arena.alloc(8, 8);
        assert!(arena.page_count() >= 2);
    }

    #[test]
    fn reserved_increases_with_each_page() {
        let mut arena = PagedArena::new();
        arena.alloc(8, 8);
        let r1 = arena.metrics().reserved_bytes;
        assert_eq!(r1, DEFAULT_PAGE_SIZE);
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        let r2 = arena.metrics().reserved_bytes;
        assert!(r2 > r1);
    }

    #[test]
    fn old_pointer_remains_valid_after_new_page() {
        let mut arena = PagedArena::new();
        let first = arena.alloc(8, 8);
        // SAFETY: pointer to arena-owned zeroed memory.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(first as *mut u64, 0xDEAD_BEEF);
        }
        for _ in 0..4 {
            arena.alloc(DEFAULT_PAGE_SIZE, 8);
        }
        // SAFETY: first pointer still valid — pages are never moved.
        #[allow(unsafe_code)]
        let value = unsafe { std::ptr::read(first as *const u64) };
        assert_eq!(value, 0xDEAD_BEEF);
    }

    #[test]
    fn allocated_memory_is_zeroed() {
        let mut arena = PagedArena::new();
        let ptr = arena.alloc(256, 8);
        // SAFETY: arena-owned zeroed memory.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(ptr, 256) };
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn write_and_read_after_multiple_allocations() {
        let mut arena = PagedArena::new();
        let a = arena.alloc(8, 8);
        let b = arena.alloc(8, 8);
        let c = arena.alloc(8, 8);
        // SAFETY: all pointers are arena-owned and non-overlapping.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(a as *mut u64, 1);
            std::ptr::write(b as *mut u64, 2);
            std::ptr::write(c as *mut u64, 3);
            assert_eq!(std::ptr::read(a as *const u64), 1);
            assert_eq!(std::ptr::read(b as *const u64), 2);
            assert_eq!(std::ptr::read(c as *const u64), 3);
        }
    }

    #[test]
    fn oversized_allocation_gets_dedicated_page() {
        let mut arena = PagedArena::new();
        arena.alloc(8, 8);
        let big_size = DEFAULT_PAGE_SIZE * 2;
        let ptr = arena.alloc(big_size, 8);
        assert!(!ptr.is_null());
        assert_eq!(ptr as usize % 8, 0);
        let m = arena.metrics();
        assert!(m.reserved_bytes >= DEFAULT_PAGE_SIZE + big_size);
        // SAFETY: verify zeroed.
        #[allow(unsafe_code)]
        let last_byte = unsafe { *ptr.add(big_size - 1) };
        assert_eq!(last_byte, 0);
    }

    #[test]
    fn allocation_near_page_limit() {
        let mut arena = PagedArena::new();
        // Page capacity = DEFAULT_PAGE_SIZE exactly (64 KiB); no slack.
        let ptr = arena.alloc(DEFAULT_PAGE_SIZE, 8);
        assert!(!ptr.is_null());
        assert_eq!(arena.metrics().used_bytes, DEFAULT_PAGE_SIZE);
        assert_eq!(arena.page_count(), 1);
        arena.alloc(MAX_ALIGN, 8);
        assert_eq!(arena.page_count(), 2);
    }

    #[test]
    fn align_up_overflow_returns_none() {
        assert!(align_up(usize::MAX, 16).is_none());
        assert!(align_up(usize::MAX - 1, 16).is_none());
    }

    #[test]
    fn drop_releases_pages_cleanly() {
        let mut arena = PagedArena::new();
        for _ in 0..10 {
            arena.alloc(DEFAULT_PAGE_SIZE, 8);
        }
        assert_eq!(arena.page_count(), 10);
        drop(arena);
    }

    #[test]
    fn empty_arena_has_zero_metrics() {
        let arena = PagedArena::new();
        let m = arena.metrics();
        assert_eq!(m.used_bytes, 0);
        assert_eq!(m.reserved_bytes, 0);
        assert_eq!(arena.page_count(), 0);
    }

    #[test]
    fn page_base_addresses_stable_across_growth() {
        let mut arena = PagedArena::new();
        let mut bases = Vec::new();
        for _ in 0..8 {
            let ptr = arena.alloc(DEFAULT_PAGE_SIZE, 8);
            bases.push(ptr);
        }
        // Allocate more to trigger Vec<Page> reallocation.
        for _ in 0..8 {
            arena.alloc(DEFAULT_PAGE_SIZE, 8);
        }
        for &base in &bases {
            // SAFETY: arena-owned zeroed memory (or written to 0 by alloc_zeroed).
            #[allow(unsafe_code)]
            let _ = unsafe { std::ptr::read(base) };
        }
    }

    #[test]
    fn two_arenas_are_fully_independent() {
        let mut a = PagedArena::new();
        let mut b = PagedArena::new();
        let pa = a.alloc(64, 8);
        let pb = b.alloc(64, 8);
        // SAFETY: write to one arena must not affect the other.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(pa as *mut u64, 0xAAAA);
            std::ptr::write(pb as *mut u64, 0xBBBB);
            assert_eq!(std::ptr::read(pa as *const u64), 0xAAAA);
            assert_eq!(std::ptr::read(pb as *const u64), 0xBBBB);
        }
        let ma = a.metrics();
        let mb = b.metrics();
        assert_eq!(ma.used_bytes, 64);
        assert_eq!(mb.used_bytes, 64);
    }

    #[test]
    #[should_panic(expected = "zero-size allocation")]
    fn alloc_zero_size_panics() {
        let mut arena = PagedArena::new();
        arena.alloc(0, 8);
    }

    #[test]
    #[should_panic(expected = "non-zero power of two")]
    fn alloc_zero_align_panics() {
        let mut arena = PagedArena::new();
        arena.alloc(8, 0);
    }

    #[test]
    #[should_panic(expected = "non-zero power of two")]
    fn alloc_nonpower_align_panics() {
        let mut arena = PagedArena::new();
        arena.alloc(8, 3);
    }

    #[test]
    #[should_panic(expected = "MAX_ALIGN")]
    fn alloc_excess_align_panics() {
        let mut arena = PagedArena::new();
        arena.alloc(8, MAX_ALIGN * 2);
    }
}
