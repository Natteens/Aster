//! Paged bump allocator for per-execution memory.
//!
//! Each [`PagedArena`] owns a sequence of pages allocated via
//! [`std::alloc::alloc_zeroed`]. Allocations bump a cursor within the last
//! active page. Rewound pages remain reserved and can be reused without moving
//! their backing memory. All pages are released when the arena is dropped.
//!
//! Regular pages grow geometrically from [`MIN_PAGE_SIZE`] through
//! [`DEFAULT_PAGE_SIZE`]. Requests larger than [`DEFAULT_PAGE_SIZE`] get a dedicated page whose
//! capacity equals the request size.

use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    sync::atomic::{AtomicU64, Ordering},
};

/// Default page capacity in bytes.
pub(crate) const DEFAULT_PAGE_SIZE: usize = 64 * 1024;

/// Initial page capacity. Small executions should not reserve a full 64 KiB
/// page before their live payload proves that capacity useful.
pub(crate) const MIN_PAGE_SIZE: usize = 4 * 1024;

/// Maximum alignment supported by arena allocations.
pub(crate) const MAX_ALIGN: usize = 16;

static NEXT_ARENA_ID: AtomicU64 = AtomicU64::new(1);

fn next_arena_id() -> u64 {
    NEXT_ARENA_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("arena id space exhausted")
}

/// One contiguous page of zeroed memory.
struct Page {
    base: *mut u8,
    layout: Layout,
    cursor: usize,
}

impl Page {
    fn try_new(capacity: usize) -> Result<Self, ArenaAllocError> {
        debug_assert!(capacity > 0);
        let layout = Layout::from_size_align(capacity, MAX_ALIGN)
            .map_err(|_| ArenaAllocError::AddressSpace)?;
        // SAFETY: `layout` has non-zero size and valid alignment. The returned
        // pointer owns `capacity` zeroed bytes.
        #[allow(unsafe_code)]
        let base = unsafe { alloc_zeroed(layout) };
        if base.is_null() {
            return Err(ArenaAllocError::OutOfMemory);
        }
        Ok(Page {
            base,
            layout,
            cursor: 0,
        })
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

    fn rewind_to(&mut self, cursor: usize) {
        assert!(
            cursor <= self.cursor,
            "page rewind cursor exceeds current cursor"
        );

        let reclaimed = self.cursor - cursor;
        if reclaimed != 0 {
            // SAFETY: `cursor <= self.cursor <= self.capacity()`, so the range
            // starting at `base + cursor` with length `reclaimed` lies entirely
            // inside this live page allocation.
            #[allow(unsafe_code)]
            unsafe {
                std::ptr::write_bytes(self.base.add(cursor), 0, reclaimed);
            }
        }

        self.cursor = cursor;
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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ArenaMark {
    arena_id: u64,
    mark_id: u64,
    active_pages: usize,
    cursor: usize,
    used_bytes: usize,
}

/// Current allocation statistics from the arena.
pub(crate) struct ArenaMetrics {
    /// Total bytes consumed inside pages, including alignment padding.
    pub used_bytes: usize,
    /// Total capacity of all pages held by the arena.
    pub reserved_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArenaAllocError {
    AddressSpace,
    Limit,
    OutOfMemory,
}

/// Paged bump allocator. Owns all pages and frees them on drop.
pub(crate) struct PagedArena {
    pages: Vec<Page>,
    active_pages: usize,
    used_bytes: usize,
    reserved_bytes: usize,
    arena_id: u64,
    next_mark_id: u64,
    mark_stack: Vec<u64>,
}

impl PagedArena {
    pub(crate) fn new() -> Self {
        PagedArena {
            pages: Vec::new(),
            active_pages: 0,
            used_bytes: 0,
            reserved_bytes: 0,
            arena_id: next_arena_id(),
            next_mark_id: 1,
            mark_stack: Vec::new(),
        }
    }

    /// Fallibly allocate zeroed arena storage without invoking Rust's global
    /// allocation-error handler. `max_reserved_bytes` is the caller-owned
    /// execution budget available to this arena.
    pub(crate) fn try_alloc(
        &mut self,
        size: usize,
        align: usize,
        max_reserved_bytes: usize,
    ) -> Result<*mut u8, ArenaAllocError> {
        if let Some(pointer) = self.try_alloc_existing(size, align)? {
            return Ok(pointer);
        }

        assert!(
            self.pages[self.active_pages..]
                .iter()
                .all(|page| page.cursor == 0),
            "inactive arena page must have zero cursor"
        );

        let reusable_index = self.pages[self.active_pages..]
            .iter()
            .enumerate()
            .filter(|(_, page)| page.capacity() >= size)
            .min_by_key(|(_, page)| page.capacity())
            .map(|(offset, _)| self.active_pages + offset);

        if let Some(index) = reusable_index {
            self.pages.swap(self.active_pages, index);
            let page = &mut self.pages[self.active_pages];
            let (ptr, consumed) = page
                .try_alloc(size, align)
                .expect("reused page must satisfy the allocation");
            self.active_pages += 1;
            self.used_bytes = self
                .used_bytes
                .checked_add(consumed)
                .ok_or(ArenaAllocError::AddressSpace)?;
            return Ok(ptr);
        }

        // Regular pages grow geometrically to keep tiny contexts dense without
        // turning sustained allocation into a long list of 4 KiB pages.
        // Dedicated oversized pages do not influence later regular growth.
        let regular_capacity = self.pages[..self.active_pages]
            .iter()
            .map(Page::capacity)
            .filter(|capacity| *capacity <= DEFAULT_PAGE_SIZE)
            .max()
            .map_or(MIN_PAGE_SIZE, |capacity| {
                capacity.saturating_mul(2).min(DEFAULT_PAGE_SIZE)
            });
        // Fresh pages are MAX_ALIGN-aligned, so the first allocation needs no padding.
        let capacity = size.max(regular_capacity);
        let new_reserved = self
            .reserved_bytes
            .checked_add(capacity)
            .ok_or(ArenaAllocError::AddressSpace)?;
        if new_reserved > max_reserved_bytes {
            return Err(ArenaAllocError::Limit);
        }
        self.pages
            .try_reserve(1)
            .map_err(|_| ArenaAllocError::OutOfMemory)?;
        let mut page = Page::try_new(capacity)?;
        let (ptr, consumed) = page
            .try_alloc(size, align)
            .expect("fresh page must satisfy the allocation");

        self.used_bytes = self
            .used_bytes
            .checked_add(consumed)
            .ok_or(ArenaAllocError::AddressSpace)?;
        self.reserved_bytes = new_reserved;
        self.pages.push(page);
        let new_page_index = self.pages.len() - 1;
        self.pages.swap(self.active_pages, new_page_index);
        self.active_pages += 1;
        Ok(ptr)
    }

    /// Allocate only when the current active page already has capacity.
    /// No page is reserved here, so callers may use this as a budget-free hot
    /// path before consulting the execution-wide reservation limit.
    #[inline]
    pub(crate) fn try_alloc_existing(
        &mut self,
        size: usize,
        align: usize,
    ) -> Result<Option<*mut u8>, ArenaAllocError> {
        assert!(size > 0, "zero-size allocation");
        assert!(
            align.is_power_of_two(),
            "alignment must be a non-zero power of two"
        );
        assert!(
            align <= MAX_ALIGN,
            "alignment {align} exceeds MAX_ALIGN ({MAX_ALIGN})"
        );

        if self.active_pages != 0 {
            let page = &mut self.pages[self.active_pages - 1];
            if let Some((ptr, consumed)) = page.try_alloc(size, align) {
                self.used_bytes = self
                    .used_bytes
                    .checked_add(consumed)
                    .ok_or(ArenaAllocError::AddressSpace)?;
                return Ok(Some(ptr));
            }
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(crate) fn alloc(&mut self, size: usize, align: usize) -> *mut u8 {
        self.try_alloc(size, align, usize::MAX)
            .expect("test arena allocation")
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn mark(&mut self) -> ArenaMark {
        let mark_id = self.next_mark_id;
        self.next_mark_id = self
            .next_mark_id
            .checked_add(1)
            .expect("arena mark id space exhausted");
        self.mark_stack.push(mark_id);

        let cursor = if self.active_pages == 0 {
            0
        } else {
            self.pages[self.active_pages - 1].cursor
        };

        ArenaMark {
            arena_id: self.arena_id,
            mark_id,
            active_pages: self.active_pages,
            cursor,
            used_bytes: self.used_bytes,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn rewind(&mut self, mark: ArenaMark) {
        let ArenaMark {
            arena_id,
            mark_id,
            active_pages,
            cursor,
            used_bytes,
        } = mark;

        assert_eq!(
            arena_id, self.arena_id,
            "arena mark belongs to a different arena"
        );

        let active_mark = self
            .mark_stack
            .last()
            .copied()
            .expect("arena has no active mark");

        assert_eq!(
            mark_id, active_mark,
            "arena marks must be rewound in LIFO order"
        );
        assert!(
            active_pages <= self.active_pages,
            "arena mark has invalid active page count"
        );
        assert!(
            used_bytes <= self.used_bytes,
            "arena mark has invalid used byte count"
        );

        if active_pages == 0 {
            assert_eq!(cursor, 0, "empty arena mark must have a zero cursor");
            assert_eq!(used_bytes, 0, "empty arena mark must have zero used bytes");

            for page in &mut self.pages[..self.active_pages] {
                page.rewind_to(0);
            }
        } else {
            assert!(
                active_pages <= self.pages.len(),
                "arena mark references a missing page"
            );

            let marked_page = active_pages - 1;

            assert!(
                cursor <= self.pages[marked_page].cursor,
                "arena mark cursor exceeds current page cursor"
            );

            self.pages[marked_page].rewind_to(cursor);

            for page in &mut self.pages[active_pages..self.active_pages] {
                page.rewind_to(0);
            }
        }

        self.active_pages = active_pages;
        self.used_bytes = used_bytes;

        let removed = self.mark_stack.pop();

        assert_eq!(
            removed,
            Some(mark_id),
            "arena mark stack changed during rewind"
        );
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

    #[cfg(test)]
    fn active_page_count(&self) -> usize {
        self.active_pages
    }

    #[cfg(test)]
    fn active_page_capacity(&self) -> Option<usize> {
        self.active_pages
            .checked_sub(1)
            .map(|index| self.pages[index].capacity())
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
    fn fallible_allocation_rejects_the_budget_before_reserving_a_page() {
        let mut arena = PagedArena::new();
        assert_eq!(
            arena.try_alloc(DEFAULT_PAGE_SIZE + 1, 8, DEFAULT_PAGE_SIZE),
            Err(ArenaAllocError::Limit)
        );
        assert_eq!(arena.page_count(), 0);
        assert_eq!(arena.metrics().reserved_bytes, 0);
    }

    #[test]
    fn first_allocation_returns_aligned_non_null_pointer() {
        let mut arena = PagedArena::new();
        let ptr = arena.alloc(32, 8);
        assert!(!ptr.is_null());
        assert_eq!(ptr as usize % 8, 0);
        assert_eq!(arena.page_count(), 1);
    }

    #[test]
    fn existing_page_fast_path_never_reserves_or_reuses_a_page() {
        let mut arena = PagedArena::new();
        assert_eq!(arena.try_alloc_existing(8, 8), Ok(None));
        let first = arena.alloc(8, 8);
        let reserved = arena.metrics().reserved_bytes;
        let second = arena
            .try_alloc_existing(8, 8)
            .expect("existing allocation is addressable")
            .expect("active page has capacity");
        assert_ne!(first, second);
        assert_eq!(arena.metrics().reserved_bytes, reserved);
        assert_eq!(
            arena.try_alloc_existing(MIN_PAGE_SIZE, 8),
            Ok(None),
            "a full active page must fall back to the budgeted slow path"
        );
        assert_eq!(arena.metrics().reserved_bytes, reserved);
    }

    #[test]
    fn multiple_small_allocations_share_growing_pages() {
        let mut arena = PagedArena::new();
        let ptrs: Vec<*mut u8> = (0..100).map(|_| arena.alloc(64, 8)).collect();
        assert_eq!(arena.page_count(), 2);
        assert_eq!(arena.metrics().reserved_bytes, MIN_PAGE_SIZE * 3);
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
        assert_eq!(r1, MIN_PAGE_SIZE);
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
        assert!(m.reserved_bytes >= MIN_PAGE_SIZE + big_size);
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

    #[test]
    fn empty_mark_rewinds_without_allocating_pages() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();
        arena.rewind(mark);

        let metrics = arena.metrics();
        assert_eq!(metrics.used_bytes, 0);
        assert_eq!(metrics.reserved_bytes, 0);
        assert_eq!(arena.page_count(), 0);
        assert_eq!(arena.active_page_count(), 0);
    }

    #[test]
    fn rewind_restores_usage_within_the_same_page() {
        let mut arena = PagedArena::new();
        arena.alloc(8, 8);
        let mark = arena.mark();
        let before = arena.metrics();

        arena.alloc(32, 8);
        assert!(arena.metrics().used_bytes > before.used_bytes);
        arena.rewind(mark);

        let after = arena.metrics();
        assert_eq!(after.used_bytes, before.used_bytes);
        assert_eq!(after.reserved_bytes, before.reserved_bytes);
        assert_eq!(arena.active_page_count(), 1);
    }

    #[test]
    fn rewind_preserves_content_allocated_before_the_mark() {
        let mut arena = PagedArena::new();
        let persistent = arena.alloc(8, 8);
        // SAFETY: `persistent` points to eight live arena-owned bytes.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(persistent as *mut u64, 0xCAFE_BABE);
        }

        let mark = arena.mark();
        arena.alloc(256, 8);
        arena.rewind(mark);

        // SAFETY: the allocation was created before the mark and remains active.
        #[allow(unsafe_code)]
        let value = unsafe { std::ptr::read(persistent as *const u64) };
        assert_eq!(value, 0xCAFE_BABE);
    }

    #[test]
    fn reused_region_is_zeroed_after_rewind() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();
        let old_address = {
            let ptr = arena.alloc(64, 8);
            // SAFETY: `ptr` points to 64 live arena-owned bytes.
            #[allow(unsafe_code)]
            unsafe {
                std::ptr::write_bytes(ptr, 0xAB, 64);
            }
            ptr as usize
        };

        arena.rewind(mark);

        let reused = arena.alloc(64, 8);
        assert_eq!(reused as usize, old_address);
        // SAFETY: `reused` is a new live allocation of 64 bytes.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(reused, 64) };
        assert!(bytes.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn rewind_restores_alignment_padding() {
        let mut arena = PagedArena::new();
        arena.alloc(3, 1);
        let mark = arena.mark();
        let before = arena.metrics().used_bytes;

        arena.alloc(4, 8);
        assert_eq!(arena.metrics().used_bytes, 12);
        arena.rewind(mark);

        assert_eq!(arena.metrics().used_bytes, before);
        assert_eq!(before, 3);
    }

    #[test]
    fn rewind_crossing_pages_keeps_them_reserved_and_inactive() {
        let mut arena = PagedArena::new();
        arena.alloc(8, 8);
        let mark = arena.mark();
        let used_before = arena.metrics().used_bytes;

        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        let reserved_before = arena.metrics().reserved_bytes;
        assert_eq!(arena.active_page_count(), 3);

        arena.rewind(mark);

        assert_eq!(arena.metrics().used_bytes, used_before);
        assert_eq!(arena.metrics().reserved_bytes, reserved_before);
        assert_eq!(arena.page_count(), 3);
        assert_eq!(arena.active_page_count(), 1);
    }

    #[test]
    fn inactive_pages_are_reused_before_allocating_new_pages() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();

        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        let page_count = arena.page_count();
        let reserved = arena.metrics().reserved_bytes;

        arena.rewind(mark);
        arena.alloc(8, 8);

        assert_eq!(arena.page_count(), page_count);
        assert_eq!(arena.metrics().reserved_bytes, reserved);
        assert_eq!(arena.active_page_count(), 1);
    }

    #[test]
    fn oversized_inactive_page_is_reused() {
        let mut arena = PagedArena::new();
        let big_size = DEFAULT_PAGE_SIZE * 2;
        let mark = arena.mark();
        let original = arena.alloc(big_size, 8) as usize;
        let reserved = arena.metrics().reserved_bytes;

        arena.rewind(mark);
        let reused = arena.alloc(big_size, 8) as usize;

        assert_eq!(reused, original);
        assert_eq!(arena.page_count(), 1);
        assert_eq!(arena.metrics().reserved_bytes, reserved);
    }

    #[test]
    fn inactive_page_reuse_uses_smallest_sufficient_capacity() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();

        arena.alloc(DEFAULT_PAGE_SIZE * 2, 8);
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        arena.rewind(mark);

        arena.alloc(32, 8);

        assert_eq!(arena.active_page_capacity(), Some(DEFAULT_PAGE_SIZE));
        assert_eq!(arena.page_count(), 2);
    }

    #[test]
    fn nested_marks_restore_only_their_own_regions() {
        let mut arena = PagedArena::new();
        let a = arena.alloc(8, 8);
        // SAFETY: `a` points to eight live arena-owned bytes.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(a as *mut u64, 1);
        }

        let outer = arena.mark();
        let b = arena.alloc(8, 8);
        // SAFETY: `b` points to eight live arena-owned bytes.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(b as *mut u64, 2);
        }

        let inner = arena.mark();
        let c_address = arena.alloc(8, 8) as usize;
        arena.rewind(inner);

        // SAFETY: `a` and `b` were allocated before the inner mark.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read(a as *const u64), 1);
            assert_eq!(std::ptr::read(b as *const u64), 2);
        }

        let reused_c = arena.alloc(8, 8);
        assert_eq!(reused_c as usize, c_address);
        arena.rewind(outer);

        // SAFETY: `a` was allocated before the outer mark.
        #[allow(unsafe_code)]
        let a_value = unsafe { std::ptr::read(a as *const u64) };
        assert_eq!(a_value, 1);
    }

    #[test]
    fn inner_rewind_does_not_clear_outer_allocation() {
        let mut arena = PagedArena::new();
        let outer = arena.mark();
        let outer_value = arena.alloc(8, 8);
        // SAFETY: `outer_value` points to eight live arena-owned bytes.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(outer_value as *mut u64, 42);
        }

        let inner = arena.mark();
        arena.alloc(128, 8);
        arena.rewind(inner);

        // SAFETY: `outer_value` was allocated before the inner mark.
        #[allow(unsafe_code)]
        let value = unsafe { std::ptr::read(outer_value as *const u64) };
        assert_eq!(value, 42);

        arena.rewind(outer);
    }

    #[test]
    #[should_panic(expected = "arena marks must be rewound in LIFO order")]
    fn rewinding_marks_out_of_order_panics() {
        let mut arena = PagedArena::new();
        let outer = arena.mark();
        let _inner = arena.mark();
        arena.rewind(outer);
    }

    #[test]
    #[should_panic(expected = "arena mark belongs to a different arena")]
    fn rewinding_mark_from_another_arena_panics() {
        let mut first = PagedArena::new();
        let mut second = PagedArena::new();
        let mark = first.mark();
        second.rewind(mark);
    }

    #[test]
    fn reserved_bytes_never_decrease_after_rewind() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        arena.alloc(DEFAULT_PAGE_SIZE * 2, 8);
        let reserved = arena.metrics().reserved_bytes;

        arena.rewind(mark);

        assert_eq!(arena.metrics().reserved_bytes, reserved);
        assert_eq!(arena.metrics().used_bytes, 0);
    }

    #[test]
    fn mark_after_multiple_pages_preserves_all_prior_pages() {
        let mut arena = PagedArena::new();
        let first = arena.alloc(DEFAULT_PAGE_SIZE, 8);
        let second = arena.alloc(DEFAULT_PAGE_SIZE, 8);
        // SAFETY: both pointers refer to live allocations created before the mark.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write(first as *mut u64, 11);
            std::ptr::write(second as *mut u64, 22);
        }

        let mark = arena.mark();
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        arena.rewind(mark);

        // SAFETY: both allocations predate the mark and remain active.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read(first as *const u64), 11);
            assert_eq!(std::ptr::read(second as *const u64), 22);
        }
        assert_eq!(arena.active_page_count(), 2);
    }

    #[test]
    fn rewind_from_exact_page_limit_reuses_following_page() {
        let mut arena = PagedArena::new();
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        let mark = arena.mark();

        arena.alloc(8, 8);
        let pages = arena.page_count();
        let reserved = arena.metrics().reserved_bytes;
        arena.rewind(mark);

        arena.alloc(8, 8);

        assert_eq!(arena.page_count(), pages);
        assert_eq!(arena.metrics().reserved_bytes, reserved);
        assert_eq!(arena.active_page_count(), 2);
    }

    #[test]
    fn drop_after_multiple_rewinds_does_not_double_free() {
        let mut arena = PagedArena::new();

        let outer = arena.mark();
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        let inner = arena.mark();
        arena.alloc(DEFAULT_PAGE_SIZE * 2, 8);
        arena.rewind(inner);
        arena.alloc(DEFAULT_PAGE_SIZE * 2, 8);
        arena.rewind(outer);

        drop(arena);
    }
}
