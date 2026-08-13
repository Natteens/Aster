//! Paged bump allocator for per-execution memory.
//!
//! Each [`PagedArena`] owns a sequence of pages backed by the runtime-private
//! [`PageBackend`]. Allocations bump a cursor within the last
//! active page. Rewound pages remain reserved and can be reused without moving
//! their backing memory. All pages are released when the arena is dropped.
//!
//! Regular pages grow geometrically from [`MIN_PAGE_SIZE`] through
//! [`DEFAULT_PAGE_SIZE`]. Requests larger than [`DEFAULT_PAGE_SIZE`] get a dedicated page whose
//! capacity equals the request size.

use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::memory_governor::{GovernorReservation, MemoryGovernor, MemoryGovernorTelemetry};

#[cfg(windows)]
use std::{ffi::c_void, mem::MaybeUninit};
#[cfg(windows)]
use windows_sys::Win32::System::{
    Memory::{
        MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
        VirtualFree,
    },
    SystemInformation::{GetSystemInfo, SYSTEM_INFO},
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

/// One stable, zeroed host allocation owned by a [`PageBackend`].
///
/// The backing keeps its exact layout so the backend can release it exactly
/// once when the page drops. It is intentionally private: page allocation
/// mechanics are a runtime concern, not an arena allocation policy.
struct PageBacking {
    base: *mut u8,
    logical_capacity: usize,
    #[cfg_attr(windows, allow(dead_code))]
    #[cfg(windows)]
    allocation_size: usize,
    system_layout: Option<Layout>,
}

impl PageBacking {
    fn capacity(&self) -> usize {
        self.logical_capacity
    }
}

/// Runtime-private mechanism for the stable zeroed backing of one arena page.
///
/// A successful allocation returns an exact-logical-capacity, aligned, writable,
/// zeroed backing whose address is stable until this object releases it.
trait PageBackend {
    fn allocate_zeroed(
        &self,
        capacity: usize,
        alignment: usize,
    ) -> Result<PageBacking, ArenaAllocError>;

    fn release(&self, backing: PageBacking);

    #[cfg(test)]
    fn is_windows_virtual(&self) -> bool {
        false
    }
}

/// Current fallback backend: the exact system allocator mechanics previously
/// owned directly by [`Page`].
#[cfg_attr(windows, allow(dead_code))]
struct SystemAllocatorPageBackend;

impl PageBackend for SystemAllocatorPageBackend {
    fn allocate_zeroed(
        &self,
        capacity: usize,
        alignment: usize,
    ) -> Result<PageBacking, ArenaAllocError> {
        let layout = Layout::from_size_align(capacity, alignment)
            .map_err(|_| ArenaAllocError::AddressSpace)?;
        // SAFETY: `layout` has non-zero size and valid alignment.
        #[allow(unsafe_code)]
        let base = unsafe { alloc_zeroed(layout) };
        if base.is_null() {
            return Err(ArenaAllocError::OutOfMemory);
        }
        Ok(PageBacking {
            base,
            logical_capacity: capacity,
            #[cfg(windows)]
            allocation_size: capacity,
            system_layout: Some(layout),
        })
    }

    fn release(&self, backing: PageBacking) {
        let layout = backing
            .system_layout
            .expect("system allocator backing has its allocation layout");
        // SAFETY: this backend created `backing.base` with exactly
        // `layout`; ownership is transferred here exactly once.
        #[allow(unsafe_code)]
        unsafe {
            dealloc(backing.base, layout);
        }
    }
}

#[cfg_attr(windows, allow(dead_code))]
static SYSTEM_ALLOCATOR_PAGE_BACKEND: SystemAllocatorPageBackend = SystemAllocatorPageBackend;

#[cfg(windows)]
/// Windows native virtual-memory backing for one stable arena page.
struct WindowsVirtualPageBackend;

#[cfg(windows)]
impl WindowsVirtualPageBackend {
    fn allocation_size(capacity: usize) -> Result<usize, ArenaAllocError> {
        let page_size = windows_page_size()?;
        capacity
            .checked_add(page_size - 1)
            .map(|value| value / page_size * page_size)
            .ok_or(ArenaAllocError::AddressSpace)
    }

    #[allow(dead_code)]
    fn decommit(backing: &PageBacking) -> Result<(), ArenaAllocError> {
        debug_assert!(backing.system_layout.is_none());
        if virtual_free(backing.base, backing.allocation_size, MEM_DECOMMIT) {
            Ok(())
        } else {
            Err(ArenaAllocError::OutOfMemory)
        }
    }

    #[allow(dead_code)]
    fn recommit(backing: &PageBacking) -> Result<(), ArenaAllocError> {
        debug_assert!(backing.system_layout.is_none());
        let committed = virtual_alloc(
            backing.base.cast::<c_void>(),
            backing.allocation_size,
            MEM_COMMIT,
        );
        if committed.is_null() {
            return Err(ArenaAllocError::OutOfMemory);
        }
        if committed.cast::<u8>() != backing.base {
            return Err(ArenaAllocError::AddressSpace);
        }
        Ok(())
    }

    fn release_backing(backing: &PageBacking) -> bool {
        debug_assert!(backing.system_layout.is_none());
        virtual_free(backing.base, 0, MEM_RELEASE)
    }
}

#[cfg(windows)]
impl PageBackend for WindowsVirtualPageBackend {
    fn allocate_zeroed(
        &self,
        capacity: usize,
        alignment: usize,
    ) -> Result<PageBacking, ArenaAllocError> {
        let allocation_size = Self::allocation_size(capacity)?;
        let base = virtual_alloc(std::ptr::null(), capacity, MEM_RESERVE | MEM_COMMIT);
        if base.is_null() {
            return Err(ArenaAllocError::OutOfMemory);
        }
        if base as usize % alignment != 0 {
            let released = virtual_free(base.cast::<u8>(), 0, MEM_RELEASE);
            debug_assert!(released, "misaligned Windows allocation must release");
            return Err(ArenaAllocError::AddressSpace);
        }
        Ok(PageBacking {
            base: base.cast::<u8>(),
            logical_capacity: capacity,
            allocation_size,
            system_layout: None,
        })
    }

    fn release(&self, backing: PageBacking) {
        let released = Self::release_backing(&backing);
        debug_assert!(released, "Windows virtual page release must succeed");
    }

    #[cfg(test)]
    fn is_windows_virtual(&self) -> bool {
        true
    }
}

#[cfg(windows)]
fn windows_page_size() -> Result<usize, ArenaAllocError> {
    let mut information = MaybeUninit::<SYSTEM_INFO>::zeroed();
    // SAFETY: GetSystemInfo initializes the supplied SYSTEM_INFO storage.
    #[allow(unsafe_code)]
    unsafe {
        GetSystemInfo(information.as_mut_ptr());
    }
    // SAFETY: GetSystemInfo initialized `information` above.
    #[allow(unsafe_code)]
    let page_size = unsafe { information.assume_init() }.dwPageSize as usize;
    if page_size == 0 || !page_size.is_power_of_two() {
        return Err(ArenaAllocError::AddressSpace);
    }
    Ok(page_size)
}

#[cfg(windows)]
fn virtual_alloc(address: *const c_void, size: usize, allocation_type: u32) -> *mut c_void {
    // SAFETY: arguments are supplied from a checked allocation contract and no
    // pointer is retained by this wrapper.
    #[allow(unsafe_code)]
    unsafe {
        VirtualAlloc(address, size, allocation_type, PAGE_READWRITE)
    }
}

#[cfg(windows)]
fn virtual_free(address: *mut u8, size: usize, free_type: u32) -> bool {
    // SAFETY: callers pass the original allocation base and the exact release
    // contract for either decommit or reservation release.
    #[allow(unsafe_code)]
    unsafe {
        VirtualFree(address.cast::<c_void>(), size, free_type) != 0
    }
}

#[cfg(windows)]
static WINDOWS_VIRTUAL_PAGE_BACKEND: WindowsVirtualPageBackend = WindowsVirtualPageBackend;

fn default_page_backend() -> &'static dyn PageBackend {
    #[cfg(windows)]
    {
        &WINDOWS_VIRTUAL_PAGE_BACKEND
    }
    #[cfg(not(windows))]
    {
        &SYSTEM_ALLOCATOR_PAGE_BACKEND
    }
}

/// RAII owner that routes release through the same backend that allocated it.
struct BackendPageBacking {
    backend: &'static dyn PageBackend,
    backing: Option<PageBacking>,
}

impl BackendPageBacking {
    fn allocate(
        backend: &'static dyn PageBackend,
        capacity: usize,
        alignment: usize,
    ) -> Result<Self, ArenaAllocError> {
        Ok(Self {
            backend,
            backing: Some(backend.allocate_zeroed(capacity, alignment)?),
        })
    }

    fn base(&self) -> *mut u8 {
        self.backing
            .as_ref()
            .expect("live page backing is present")
            .base
    }

    fn capacity(&self) -> usize {
        self.backing
            .as_ref()
            .expect("live page backing is present")
            .capacity()
    }
}

impl Drop for BackendPageBacking {
    fn drop(&mut self) {
        if let Some(backing) = self.backing.take() {
            self.backend.release(backing);
        }
    }
}

/// One contiguous page of zeroed memory.
struct Page {
    backing: BackendPageBacking,
    cursor: usize,
    _governor_reservation: Option<GovernorReservation>,
}

impl Page {
    fn try_new(
        capacity: usize,
        governor_reservation: Option<GovernorReservation>,
        backend: &'static dyn PageBackend,
    ) -> Result<Self, ArenaAllocError> {
        Self::try_new_with_backend(capacity, governor_reservation, backend)
    }

    fn try_new_with_backend(
        capacity: usize,
        governor_reservation: Option<GovernorReservation>,
        backend: &'static dyn PageBackend,
    ) -> Result<Self, ArenaAllocError> {
        debug_assert!(capacity > 0);
        Ok(Page {
            backing: BackendPageBacking::allocate(backend, capacity, MAX_ALIGN)?,
            cursor: 0,
            _governor_reservation: governor_reservation,
        })
    }

    fn capacity(&self) -> usize {
        self.backing.capacity()
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
        let ptr = unsafe { self.backing.base().add(aligned) };
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
                std::ptr::write_bytes(self.backing.base().add(cursor), 0, reclaimed);
            }
        }

        self.cursor = cursor;
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ArenaMark {
    arena_id: u64,
    mark_id: u64,
    active_pages: usize,
    cursor: usize,
    used_bytes: usize,
    #[cfg(feature = "aarm-telemetry")]
    active_page_capacity_bytes: usize,
}

/// Current allocation statistics from the arena.
pub(crate) struct ArenaMetrics {
    /// Total bytes consumed inside pages, including alignment padding.
    pub used_bytes: usize,
    /// Total capacity of all pages held by the arena.
    pub reserved_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArenaEventMetrics {
    pub active_page_fast_path_allocations: u64,
    pub slow_path_allocations: u64,
    pub inactive_page_reuse_events: u64,
    pub fresh_regular_page_allocations: u64,
    pub fresh_oversized_page_allocations: u64,
    pub rewind_events: u64,
    pub rewound_bytes: u64,
    pub allocation_limit_denials: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArenaRewindMetrics {
    pub used_bytes_before: usize,
    pub used_bytes_after: usize,
    pub capacity_bytes_before: usize,
    pub capacity_bytes_after: usize,
    pub active_page_capacity_bytes_after: usize,
    pub inactive_page_capacity_bytes_after: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArenaTelemetrySnapshot {
    pub used_bytes: usize,
    pub capacity_bytes: usize,
    pub active_page_capacity_bytes: usize,
    pub inactive_page_capacity_bytes: usize,
    pub page_count: usize,
    pub active_page_count: usize,
    pub inactive_page_count: usize,
    pub peak_used_bytes: usize,
    pub peak_capacity_bytes: usize,
    pub events: ArenaEventMetrics,
    pub last_rewind: Option<ArenaRewindMetrics>,
}

#[cfg(feature = "aarm-telemetry")]
#[derive(Default)]
struct ArenaTelemetryState {
    active_page_capacity_bytes: usize,
    peak_used_bytes: usize,
    peak_capacity_bytes: usize,
    events: ArenaEventMetrics,
    last_rewind: Option<ArenaRewindMetrics>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArenaAllocError {
    AddressSpace,
    Limit,
    SharedLimit { hard_limit_bytes: u64 },
    OutOfMemory,
}

/// Paged bump allocator. Owns all pages and frees them on drop.
pub(crate) struct PagedArena {
    pages: Vec<Page>,
    backend: &'static dyn PageBackend,
    governor: Option<Arc<MemoryGovernor>>,
    active_pages: usize,
    used_bytes: usize,
    reserved_bytes: usize,
    arena_id: u64,
    next_mark_id: u64,
    mark_stack: Vec<u64>,
    #[cfg(feature = "aarm-telemetry")]
    telemetry: ArenaTelemetryState,
}

impl PagedArena {
    pub(crate) fn new() -> Self {
        PagedArena {
            pages: Vec::new(),
            backend: default_page_backend(),
            governor: None,
            active_pages: 0,
            used_bytes: 0,
            reserved_bytes: 0,
            arena_id: next_arena_id(),
            next_mark_id: 1,
            mark_stack: Vec::new(),
            #[cfg(feature = "aarm-telemetry")]
            telemetry: ArenaTelemetryState::default(),
        }
    }

    pub(crate) fn with_memory_governor(governor: Arc<MemoryGovernor>) -> Self {
        Self {
            governor: Some(governor),
            ..Self::new()
        }
    }

    #[cfg(test)]
    fn with_backend(backend: &'static dyn PageBackend) -> Self {
        Self {
            backend,
            ..Self::new()
        }
    }

    #[cfg(test)]
    fn with_backend_and_memory_governor(
        backend: &'static dyn PageBackend,
        governor: Arc<MemoryGovernor>,
    ) -> Self {
        Self {
            backend,
            governor: Some(governor),
            ..Self::new()
        }
    }

    pub(crate) fn governor_telemetry(&self) -> Option<MemoryGovernorTelemetry> {
        self.governor.as_ref().map(|governor| governor.telemetry())
    }

    pub(crate) fn is_governed_by(&self, governor: &Arc<MemoryGovernor>) -> bool {
        self.governor
            .as_ref()
            .is_some_and(|owned| Arc::ptr_eq(owned, governor))
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

        #[cfg(feature = "aarm-telemetry")]
        {
            self.telemetry.events.slow_path_allocations += 1;
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
            #[cfg(feature = "aarm-telemetry")]
            let activated_capacity = page.capacity();
            let (ptr, consumed) = page
                .try_alloc(size, align)
                .expect("reused page must satisfy the allocation");
            self.active_pages += 1;
            self.used_bytes = self
                .used_bytes
                .checked_add(consumed)
                .ok_or(ArenaAllocError::AddressSpace)?;
            #[cfg(feature = "aarm-telemetry")]
            {
                self.telemetry.events.inactive_page_reuse_events += 1;
                self.telemetry.active_page_capacity_bytes += activated_capacity;
            }
            #[cfg(feature = "aarm-telemetry")]
            self.refresh_telemetry_peaks();
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
            #[cfg(feature = "aarm-telemetry")]
            {
                self.telemetry.events.allocation_limit_denials += 1;
            }
            return Err(ArenaAllocError::Limit);
        }
        self.pages
            .try_reserve(1)
            .map_err(|_| ArenaAllocError::OutOfMemory)?;
        let governor_reservation = if let Some(governor) = &self.governor {
            Some(
                governor
                    .try_acquire(capacity)
                    .ok_or(ArenaAllocError::SharedLimit {
                        hard_limit_bytes: governor.hard_limit_bytes(),
                    })?,
            )
        } else {
            None
        };
        let mut page = Page::try_new(capacity, governor_reservation, self.backend)?;
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
        #[cfg(feature = "aarm-telemetry")]
        {
            self.telemetry.active_page_capacity_bytes += capacity;
            if size > DEFAULT_PAGE_SIZE {
                self.telemetry.events.fresh_oversized_page_allocations += 1;
            } else {
                self.telemetry.events.fresh_regular_page_allocations += 1;
            }
        }
        #[cfg(feature = "aarm-telemetry")]
        self.refresh_telemetry_peaks();
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
                #[cfg(feature = "aarm-telemetry")]
                {
                    self.telemetry.events.active_page_fast_path_allocations += 1;
                }
                #[cfg(feature = "aarm-telemetry")]
                self.refresh_telemetry_peaks();
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
            #[cfg(feature = "aarm-telemetry")]
            active_page_capacity_bytes: self.telemetry.active_page_capacity_bytes,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn rewind(&mut self, mark: ArenaMark) {
        #[cfg(feature = "aarm-telemetry")]
        let used_bytes_before = self.used_bytes;
        #[cfg(feature = "aarm-telemetry")]
        let capacity_bytes_before = self.reserved_bytes;
        let ArenaMark {
            arena_id,
            mark_id,
            active_pages,
            cursor,
            used_bytes,
            #[cfg(feature = "aarm-telemetry")]
            active_page_capacity_bytes,
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

        #[cfg(feature = "aarm-telemetry")]
        {
            self.telemetry.active_page_capacity_bytes = active_page_capacity_bytes;
            let active_page_capacity_bytes_after = active_page_capacity_bytes;
            let inactive_page_capacity_bytes_after =
                self.reserved_bytes - active_page_capacity_bytes_after;
            self.telemetry.events.rewind_events += 1;
            self.telemetry.events.rewound_bytes += (used_bytes_before - used_bytes) as u64;
            self.telemetry.last_rewind = Some(ArenaRewindMetrics {
                used_bytes_before,
                used_bytes_after: used_bytes,
                capacity_bytes_before,
                capacity_bytes_after: self.reserved_bytes,
                active_page_capacity_bytes_after,
                inactive_page_capacity_bytes_after,
            });
        }
    }

    pub(crate) fn metrics(&self) -> ArenaMetrics {
        ArenaMetrics {
            used_bytes: self.used_bytes,
            reserved_bytes: self.reserved_bytes,
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn telemetry_snapshot(&self) -> Option<ArenaTelemetrySnapshot> {
        #[cfg(not(feature = "aarm-telemetry"))]
        return None;

        #[cfg(feature = "aarm-telemetry")]
        let active_page_capacity_bytes = self.telemetry.active_page_capacity_bytes;
        #[cfg(feature = "aarm-telemetry")]
        Some(ArenaTelemetrySnapshot {
            used_bytes: self.used_bytes,
            capacity_bytes: self.reserved_bytes,
            active_page_capacity_bytes,
            inactive_page_capacity_bytes: self.reserved_bytes - active_page_capacity_bytes,
            page_count: self.pages.len(),
            active_page_count: self.active_pages,
            inactive_page_count: self.pages.len() - self.active_pages,
            peak_used_bytes: self.telemetry.peak_used_bytes,
            peak_capacity_bytes: self.telemetry.peak_capacity_bytes,
            events: self.telemetry.events,
            last_rewind: self.telemetry.last_rewind,
        })
    }

    #[cfg(feature = "aarm-telemetry")]
    fn refresh_telemetry_peaks(&mut self) {
        self.telemetry.peak_used_bytes = self.telemetry.peak_used_bytes.max(self.used_bytes);
        self.telemetry.peak_capacity_bytes =
            self.telemetry.peak_capacity_bytes.max(self.reserved_bytes);
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
    use std::sync::{Arc, atomic::AtomicUsize};

    struct CountingPageBackend {
        fail_allocations: bool,
        allocation_calls: AtomicUsize,
        release_calls: AtomicUsize,
    }

    impl CountingPageBackend {
        fn new(fail_allocations: bool) -> Self {
            Self {
                fail_allocations,
                allocation_calls: AtomicUsize::new(0),
                release_calls: AtomicUsize::new(0),
            }
        }
    }

    impl PageBackend for CountingPageBackend {
        fn allocate_zeroed(
            &self,
            capacity: usize,
            alignment: usize,
        ) -> Result<PageBacking, ArenaAllocError> {
            self.allocation_calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_allocations {
                return Err(ArenaAllocError::OutOfMemory);
            }
            SYSTEM_ALLOCATOR_PAGE_BACKEND.allocate_zeroed(capacity, alignment)
        }

        fn release(&self, backing: PageBacking) {
            self.release_calls.fetch_add(1, Ordering::Relaxed);
            SYSTEM_ALLOCATOR_PAGE_BACKEND.release(backing);
        }
    }

    fn test_backend(fail_allocations: bool) -> &'static CountingPageBackend {
        Box::leak(Box::new(CountingPageBackend::new(fail_allocations)))
    }

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
    fn failed_host_page_allocation_releases_its_governor_grant() {
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE));
        let backend = test_backend(true);
        let mut arena =
            PagedArena::with_backend_and_memory_governor(backend, Arc::clone(&governor));
        let result = arena.try_alloc(8, 8, usize::MAX);
        assert!(matches!(result, Err(ArenaAllocError::OutOfMemory)));
        assert_eq!(backend.allocation_calls.load(Ordering::Relaxed), 1);
        assert_eq!(backend.release_calls.load(Ordering::Relaxed), 0);
        assert_eq!(arena.page_count(), 0);
        assert_eq!(arena.active_page_count(), 0);
        assert_eq!(arena.metrics().used_bytes, 0);
        assert_eq!(arena.metrics().reserved_bytes, 0);

        let telemetry = governor.telemetry();
        assert_eq!(telemetry.current_capacity_bytes, 0);
        assert_eq!(telemetry.grant_events, 1);
        assert_eq!(telemetry.release_events, 1);
        assert_eq!(
            telemetry.granted_bytes_cumulative - telemetry.released_bytes_cumulative,
            telemetry.current_capacity_bytes
        );
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
    fn system_backend_returns_zeroed_aligned_stable_backing_and_releases_it() {
        let backend = test_backend(false);
        let backing = BackendPageBacking::allocate(backend, MIN_PAGE_SIZE, MAX_ALIGN)
            .expect("system-backed page allocation succeeds");
        assert!(!backing.base().is_null());
        assert_eq!(backing.capacity(), MIN_PAGE_SIZE);
        assert_eq!(backing.base() as usize % MAX_ALIGN, 0);
        let base = backing.base();
        // SAFETY: `backing` owns a live zeroed allocation of MIN_PAGE_SIZE bytes.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(base, MIN_PAGE_SIZE) };
        assert!(bytes.iter().all(|&byte| byte == 0));
        assert_eq!(backing.base(), base);
        drop(backing);
        assert_eq!(backend.allocation_calls.load(Ordering::Relaxed), 1);
        assert_eq!(backend.release_calls.load(Ordering::Relaxed), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_backend_is_virtual_memory_backed() {
        let arena = PagedArena::new();
        assert!(arena.backend.is_windows_virtual());
    }

    #[cfg(windows)]
    #[test]
    fn windows_virtual_backing_decommits_recommits_at_its_original_zeroed_address() {
        let capacity = DEFAULT_PAGE_SIZE + 1;
        let backing = WINDOWS_VIRTUAL_PAGE_BACKEND
            .allocate_zeroed(capacity, MAX_ALIGN)
            .expect("Windows virtual allocation succeeds");
        assert_eq!(backing.capacity(), capacity);
        assert!(backing.allocation_size >= capacity);
        assert_eq!(backing.base as usize % MAX_ALIGN, 0);
        // SAFETY: `backing` owns a writable committed allocation of `capacity` bytes.
        #[allow(unsafe_code)]
        unsafe {
            let bytes = std::slice::from_raw_parts_mut(backing.base, capacity);
            assert!(bytes.iter().all(|&byte| byte == 0));
            bytes.fill(0xA5);
        }

        WindowsVirtualPageBackend::decommit(&backing).expect("decommit retains the reservation");
        // Do not dereference `backing.base` while it is decommitted.
        WindowsVirtualPageBackend::recommit(&backing)
            .expect("recommit restores the original reservation");
        // SAFETY: recommit restored the same base as committed writable memory.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(backing.base, capacity) };
        assert!(bytes.iter().all(|&byte| byte == 0));
        assert!(WindowsVirtualPageBackend::release_backing(&backing));
    }

    #[cfg(windows)]
    #[test]
    fn windows_virtual_backing_raii_drop_releases_its_reservation() {
        let backing =
            BackendPageBacking::allocate(&WINDOWS_VIRTUAL_PAGE_BACKEND, MIN_PAGE_SIZE, MAX_ALIGN)
                .expect("Windows virtual allocation succeeds");
        let base = backing.base();
        drop(backing);

        let reacquired = virtual_alloc(
            base.cast::<c_void>(),
            MIN_PAGE_SIZE,
            MEM_RESERVE | MEM_COMMIT,
        );
        assert_eq!(reacquired.cast::<u8>(), base);
        assert!(virtual_free(reacquired.cast::<u8>(), 0, MEM_RELEASE));
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_arena_preserves_exact_oversized_logical_capacity() {
        let mut arena = PagedArena::new();
        let capacity = DEFAULT_PAGE_SIZE + 1;
        let pointer = arena.alloc(capacity, MAX_ALIGN);
        assert_eq!(arena.metrics().reserved_bytes, capacity);
        // SAFETY: `pointer` is a live zeroed allocation of `capacity` bytes.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(pointer, capacity) };
        assert!(bytes.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn backend_is_used_only_for_fresh_pages_and_releases_each_backing_once() {
        let backend = test_backend(false);
        let mut arena = PagedArena::with_backend(backend);
        let first = arena.alloc(8, 8);
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        arena.alloc(DEFAULT_PAGE_SIZE * 2, 8);
        arena.alloc(8, 8);
        let allocations = backend.allocation_calls.load(Ordering::Relaxed);
        assert_eq!(allocations, 4);

        let second = arena
            .try_alloc_existing(8, 8)
            .expect("active allocation is addressable")
            .expect("current page has capacity");
        assert_ne!(first, second);
        assert_eq!(
            backend.allocation_calls.load(Ordering::Relaxed),
            allocations
        );
        drop(arena);
        assert_eq!(backend.release_calls.load(Ordering::Relaxed), allocations);
    }

    #[test]
    fn inactive_page_reuse_does_not_allocate_backing_or_reacquire_governor_capacity() {
        let backend = test_backend(false);
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE));
        let mut arena =
            PagedArena::with_backend_and_memory_governor(backend, Arc::clone(&governor));
        let mark = arena.mark();
        let first = arena.alloc(64, 8);
        // SAFETY: this allocation is live until the rewind below.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write_bytes(first, 0xAB, 64);
        }
        arena.rewind(mark);
        let allocations_before = backend.allocation_calls.load(Ordering::Relaxed);
        let grants_before = governor.telemetry().grant_events;
        let reused = arena.alloc(64, 8);
        assert_eq!(reused, first);
        assert_eq!(
            backend.allocation_calls.load(Ordering::Relaxed),
            allocations_before
        );
        assert_eq!(governor.telemetry().grant_events, grants_before);
        // SAFETY: `reused` is a live allocation whose rewind-reclaimed bytes were zeroed.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(reused, 64) };
        assert!(bytes.iter().all(|&byte| byte == 0));
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
    #[cfg(feature = "aarm-telemetry")]
    fn telemetry_classifies_fast_and_fresh_slow_paths() {
        let mut arena = PagedArena::new();
        arena.alloc(32, 8);
        arena.alloc(32, 8);

        let telemetry = arena.telemetry_snapshot().expect("telemetry enabled");
        assert_eq!(telemetry.used_bytes, 64);
        assert_eq!(telemetry.capacity_bytes, MIN_PAGE_SIZE);
        assert_eq!(telemetry.active_page_capacity_bytes, MIN_PAGE_SIZE);
        assert_eq!(telemetry.inactive_page_capacity_bytes, 0);
        assert_eq!(telemetry.page_count, 1);
        assert_eq!(telemetry.active_page_count, 1);
        assert_eq!(telemetry.inactive_page_count, 0);
        assert_eq!(telemetry.peak_used_bytes, 64);
        assert_eq!(telemetry.peak_capacity_bytes, MIN_PAGE_SIZE);
        assert_eq!(telemetry.events.slow_path_allocations, 1);
        assert_eq!(telemetry.events.active_page_fast_path_allocations, 1);
        assert_eq!(telemetry.events.fresh_regular_page_allocations, 1);
        assert_eq!(telemetry.events.fresh_oversized_page_allocations, 0);
    }

    #[test]
    #[cfg(feature = "aarm-telemetry")]
    fn telemetry_records_rewind_and_inactive_page_reuse_without_new_capacity() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        arena.alloc(DEFAULT_PAGE_SIZE, 8);
        let capacity = arena.metrics().reserved_bytes;

        arena.rewind(mark);
        let rewound = arena.telemetry_snapshot().expect("telemetry enabled");
        assert_eq!(rewound.used_bytes, 0);
        assert_eq!(rewound.capacity_bytes, capacity);
        assert_eq!(rewound.active_page_capacity_bytes, 0);
        assert_eq!(rewound.inactive_page_capacity_bytes, capacity);
        assert_eq!(rewound.events.rewind_events, 1);
        assert_eq!(rewound.events.rewound_bytes, (2 * DEFAULT_PAGE_SIZE) as u64);
        assert_eq!(
            rewound.last_rewind,
            Some(ArenaRewindMetrics {
                used_bytes_before: 2 * DEFAULT_PAGE_SIZE,
                used_bytes_after: 0,
                capacity_bytes_before: capacity,
                capacity_bytes_after: capacity,
                active_page_capacity_bytes_after: 0,
                inactive_page_capacity_bytes_after: capacity,
            })
        );

        arena.alloc(8, 8);
        let reused = arena.telemetry_snapshot().expect("telemetry enabled");
        assert_eq!(reused.capacity_bytes, capacity);
        assert_eq!(reused.events.inactive_page_reuse_events, 1);
        assert_eq!(reused.events.fresh_regular_page_allocations, 2);
    }

    #[test]
    #[cfg(feature = "aarm-telemetry")]
    fn telemetry_distinguishes_oversized_pages_and_limit_denials() {
        let mut oversized = PagedArena::new();
        oversized.alloc(DEFAULT_PAGE_SIZE + 1, 8);
        let metrics = oversized.telemetry_snapshot().expect("telemetry enabled");
        assert_eq!(metrics.events.fresh_oversized_page_allocations, 1);
        assert_eq!(metrics.events.fresh_regular_page_allocations, 0);

        let mut denied = PagedArena::new();
        assert_eq!(
            denied.try_alloc(DEFAULT_PAGE_SIZE + 1, 8, DEFAULT_PAGE_SIZE),
            Err(ArenaAllocError::Limit)
        );
        let metrics = denied.telemetry_snapshot().expect("telemetry enabled");
        assert_eq!(metrics.used_bytes, 0);
        assert_eq!(metrics.capacity_bytes, 0);
        assert_eq!(metrics.peak_used_bytes, 0);
        assert_eq!(metrics.peak_capacity_bytes, 0);
        assert_eq!(metrics.events.slow_path_allocations, 1);
        assert_eq!(metrics.events.allocation_limit_denials, 1);
    }

    #[test]
    fn page_base_addresses_stable_across_growth() {
        let backend = test_backend(false);
        let mut arena = PagedArena::with_backend(backend);
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
        let allocations = backend.allocation_calls.load(Ordering::Relaxed);
        drop(arena);
        assert_eq!(backend.release_calls.load(Ordering::Relaxed), allocations);
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
