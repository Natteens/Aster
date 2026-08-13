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

#[cfg(any(windows, target_os = "linux"))]
use std::ffi::c_void;
#[cfg(windows)]
use std::mem::MaybeUninit;
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
/// The backing keeps its exact host-allocation metadata so the backend can
/// release it exactly once when the page drops. It is intentionally private:
/// page allocation mechanics are a runtime concern, not an arena allocation
/// policy.
struct PageBacking {
    base: *mut u8,
    logical_capacity: usize,
    vm_allocation_size: Option<usize>,
    system_layout: Option<Layout>,
    state: PageBackingState,
}

impl PageBacking {
    fn capacity(&self) -> usize {
        self.logical_capacity
    }

    #[cfg(feature = "aarm-telemetry")]
    fn virtual_extent(&self) -> Option<usize> {
        self.vm_allocation_size
    }
}

/// AARM's explicit lifecycle state for one complete host backing range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageBackingState {
    Retained,
    Discarded,
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

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "AARM-4A exposes this only to future explicit runtime callers"
        )
    )]
    fn supports_discard(&self) -> bool {
        false
    }

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "AARM-4A exposes this only to future explicit runtime callers"
        )
    )]
    fn discard(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
        Err(ArenaAllocError::AddressSpace)
    }

    fn prepare_for_reuse(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
        Err(ArenaAllocError::AddressSpace)
    }

    #[cfg(feature = "aarm-telemetry")]
    fn has_known_virtual_extent(&self) -> bool {
        false
    }

    #[cfg(all(test, windows))]
    fn is_windows_virtual(&self) -> bool {
        false
    }

    #[cfg(all(test, target_os = "linux"))]
    fn is_linux_anonymous(&self) -> bool {
        false
    }
}

/// Current fallback backend: the exact system allocator mechanics previously
/// owned directly by [`Page`].
#[cfg_attr(any(windows, target_os = "linux"), allow(dead_code))]
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
            vm_allocation_size: None,
            system_layout: Some(layout),
            state: PageBackingState::Retained,
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

#[cfg_attr(any(windows, target_os = "linux"), allow(dead_code))]
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
        let allocation_size = backing
            .vm_allocation_size
            .expect("Windows virtual backing has its allocation size");
        if virtual_free(backing.base, allocation_size, MEM_DECOMMIT) {
            Ok(())
        } else {
            Err(ArenaAllocError::OutOfMemory)
        }
    }

    #[allow(dead_code)]
    fn recommit(backing: &PageBacking) -> Result<(), ArenaAllocError> {
        debug_assert!(backing.system_layout.is_none());
        let allocation_size = backing
            .vm_allocation_size
            .expect("Windows virtual backing has its allocation size");
        let committed = virtual_alloc(backing.base.cast::<c_void>(), allocation_size, MEM_COMMIT);
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
            vm_allocation_size: Some(allocation_size),
            system_layout: None,
            state: PageBackingState::Retained,
        })
    }

    fn release(&self, backing: PageBacking) {
        let released = Self::release_backing(&backing);
        debug_assert!(released, "Windows virtual page release must succeed");
    }

    fn supports_discard(&self) -> bool {
        true
    }

    fn discard(&self, backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
        Self::decommit(backing)
    }

    fn prepare_for_reuse(&self, backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
        Self::recommit(backing)
    }

    #[cfg(feature = "aarm-telemetry")]
    fn has_known_virtual_extent(&self) -> bool {
        true
    }

    #[cfg(all(test, windows))]
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

#[cfg(target_os = "linux")]
/// Linux anonymous private mapping backing for one stable arena page.
struct LinuxAnonymousPageBackend;

#[cfg(target_os = "linux")]
impl LinuxAnonymousPageBackend {
    fn mapping_size(capacity: usize) -> Result<usize, ArenaAllocError> {
        let page_size = linux_page_size()?;
        capacity
            .checked_add(page_size - 1)
            .map(|value| value / page_size * page_size)
            .ok_or(ArenaAllocError::AddressSpace)
    }

    #[allow(dead_code)]
    fn discard(backing: &PageBacking) -> Result<(), ArenaAllocError> {
        debug_assert!(backing.system_layout.is_none());
        let mapping_size = backing
            .vm_allocation_size
            .expect("Linux anonymous backing has its mapping size");
        if linux_madvise_dontneed(backing.base, mapping_size) {
            Ok(())
        } else {
            Err(ArenaAllocError::OutOfMemory)
        }
    }

    fn release_backing(backing: &PageBacking) -> bool {
        debug_assert!(backing.system_layout.is_none());
        let mapping_size = backing
            .vm_allocation_size
            .expect("Linux anonymous backing has its mapping size");
        linux_munmap(backing.base, mapping_size)
    }
}

#[cfg(target_os = "linux")]
impl PageBackend for LinuxAnonymousPageBackend {
    fn allocate_zeroed(
        &self,
        capacity: usize,
        alignment: usize,
    ) -> Result<PageBacking, ArenaAllocError> {
        let mapping_size = Self::mapping_size(capacity)?;
        let base = linux_mmap(mapping_size);
        if base == libc::MAP_FAILED {
            return Err(ArenaAllocError::OutOfMemory);
        }
        if base as usize % alignment != 0 {
            let released = linux_munmap(base.cast::<u8>(), mapping_size);
            debug_assert!(released, "misaligned Linux mapping must release");
            return Err(ArenaAllocError::AddressSpace);
        }
        Ok(PageBacking {
            base: base.cast::<u8>(),
            logical_capacity: capacity,
            vm_allocation_size: Some(mapping_size),
            system_layout: None,
            state: PageBackingState::Retained,
        })
    }

    fn release(&self, backing: PageBacking) {
        let released = Self::release_backing(&backing);
        debug_assert!(released, "Linux anonymous page release must succeed");
    }

    fn supports_discard(&self) -> bool {
        true
    }

    fn discard(&self, backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
        Self::discard(backing)
    }

    fn prepare_for_reuse(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
        Ok(())
    }

    #[cfg(feature = "aarm-telemetry")]
    fn has_known_virtual_extent(&self) -> bool {
        true
    }

    #[cfg(all(test, target_os = "linux"))]
    fn is_linux_anonymous(&self) -> bool {
        true
    }
}

#[cfg(target_os = "linux")]
fn linux_page_size() -> Result<usize, ArenaAllocError> {
    // SAFETY: `_SC_PAGESIZE` takes no pointer and returns the host page size or
    // a negative error result.
    #[allow(unsafe_code)]
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return Err(ArenaAllocError::AddressSpace);
    }
    usize::try_from(page_size).map_err(|_| ArenaAllocError::AddressSpace)
}

#[cfg(target_os = "linux")]
fn linux_mmap(mapping_size: usize) -> *mut c_void {
    // SAFETY: this creates one anonymous private writable mapping with no file
    // descriptor ownership; the returned base is retained only by PageBacking.
    #[allow(unsafe_code)]
    unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            mapping_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    }
}

#[cfg(target_os = "linux")]
fn linux_munmap(base: *mut u8, mapping_size: usize) -> bool {
    // SAFETY: callers pass exactly the live mapping base and mapping extent
    // recorded when mmap succeeded.
    #[allow(unsafe_code)]
    unsafe {
        libc::munmap(base.cast::<c_void>(), mapping_size) == 0
    }
}

#[cfg(target_os = "linux")]
fn linux_madvise_dontneed(base: *mut u8, mapping_size: usize) -> bool {
    // SAFETY: callers pass exactly one still-owned, page-aligned anonymous
    // mapping range; MADV_DONTNEED keeps that mapping addressable.
    #[allow(unsafe_code)]
    unsafe {
        libc::madvise(base.cast::<c_void>(), mapping_size, libc::MADV_DONTNEED) == 0
    }
}

#[cfg(target_os = "linux")]
static LINUX_ANONYMOUS_PAGE_BACKEND: LinuxAnonymousPageBackend = LinuxAnonymousPageBackend;

fn default_page_backend() -> &'static dyn PageBackend {
    #[cfg(windows)]
    {
        &WINDOWS_VIRTUAL_PAGE_BACKEND
    }
    #[cfg(target_os = "linux")]
    {
        &LINUX_ANONYMOUS_PAGE_BACKEND
    }
    #[cfg(not(any(windows, target_os = "linux")))]
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

    fn state(&self) -> PageBackingState {
        self.backing
            .as_ref()
            .expect("live page backing is present")
            .state
    }

    #[cfg(feature = "aarm-telemetry")]
    fn virtual_extent(&self) -> Option<usize> {
        self.backing
            .as_ref()
            .expect("live page backing is present")
            .virtual_extent()
    }

    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "AARM-4A exposes this only to future explicit runtime callers"
        )
    )]
    fn discard(&mut self) -> Result<(), ArenaAllocError> {
        let backing = self.backing.as_mut().expect("live page backing is present");
        assert_eq!(
            backing.state,
            PageBackingState::Retained,
            "only retained backing may be discarded"
        );
        self.backend.discard(backing)?;
        backing.state = PageBackingState::Discarded;
        Ok(())
    }

    fn prepare_for_reuse(&mut self) -> Result<(), ArenaAllocError> {
        let backing = self.backing.as_mut().expect("live page backing is present");
        assert_eq!(
            backing.state,
            PageBackingState::Discarded,
            "only discarded backing needs reuse preparation"
        );
        let base = backing.base;
        self.backend.prepare_for_reuse(backing)?;
        if backing.base != base {
            return Err(ArenaAllocError::AddressSpace);
        }
        backing.state = PageBackingState::Retained;
        Ok(())
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

    fn backing_state(&self) -> PageBackingState {
        self.backing.state()
    }

    #[cfg(feature = "aarm-telemetry")]
    fn virtual_extent(&self) -> Option<usize> {
        self.backing.virtual_extent()
    }

    fn prepare_for_reuse(&mut self) -> Result<(), ArenaAllocError> {
        if self.backing_state() == PageBackingState::Discarded {
            self.backing.prepare_for_reuse()?;
        }
        debug_assert_eq!(self.backing_state(), PageBackingState::Retained);
        Ok(())
    }

    /// Try to place `size` bytes at the given alignment inside this page.
    /// Returns the pointer and the number of bytes consumed (including padding)
    /// on success.
    fn try_alloc(&mut self, size: usize, align: usize) -> Option<(*mut u8, usize)> {
        debug_assert_eq!(self.backing_state(), PageBackingState::Retained);
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

/// Deterministic result of one explicit caller-owned inactive-page purge.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "AARM-4A exposes this only to future explicit runtime callers"
    )
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArenaPurgeReport {
    pub examined: usize,
    pub eligible: usize,
    pub discarded: usize,
    pub failed: usize,
    pub already_discarded: usize,
    pub unsupported: usize,
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
    pub virtual_extent_bytes: Option<usize>,
    pub backing_retained_bytes: Option<usize>,
    pub backing_discarded_bytes: Option<usize>,
    pub peak_backing_retained_bytes: Option<usize>,
    pub events: ArenaEventMetrics,
    pub last_rewind: Option<ArenaRewindMetrics>,
}

#[cfg(feature = "aarm-telemetry")]
#[derive(Default)]
struct ArenaTelemetryState {
    active_page_capacity_bytes: usize,
    peak_used_bytes: usize,
    peak_capacity_bytes: usize,
    peak_backing_retained_bytes: Option<usize>,
    events: ArenaEventMetrics,
    last_rewind: Option<ArenaRewindMetrics>,
}

#[cfg(feature = "aarm-telemetry")]
#[derive(Default)]
struct BackingTelemetry {
    virtual_extent: Option<usize>,
    retained: Option<usize>,
    discarded: Option<usize>,
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
            telemetry: ArenaTelemetryState {
                peak_backing_retained_bytes: default_page_backend()
                    .has_known_virtual_extent()
                    .then_some(0),
                ..ArenaTelemetryState::default()
            },
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
        #[cfg(feature = "aarm-telemetry")]
        let mut arena = Self {
            backend,
            ..Self::new()
        };
        #[cfg(not(feature = "aarm-telemetry"))]
        let arena = Self {
            backend,
            ..Self::new()
        };
        #[cfg(feature = "aarm-telemetry")]
        {
            arena.telemetry.peak_backing_retained_bytes =
                backend.has_known_virtual_extent().then_some(0);
        }
        arena
    }

    #[cfg(test)]
    fn with_backend_and_memory_governor(
        backend: &'static dyn PageBackend,
        governor: Arc<MemoryGovernor>,
    ) -> Self {
        let mut arena = Self::with_backend(backend);
        arena.governor = Some(governor);
        arena
    }

    /// Discard every fully inactive retained backing supported by this arena's
    /// backend. This is an explicit caller-owned operation; normal rewind and
    /// allocation paths never invoke it automatically.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "AARM-4A deliberately has no automatic purge caller"
        )
    )]
    pub(crate) fn purge_inactive_pages(&mut self) -> ArenaPurgeReport {
        assert!(
            self.pages[self.active_pages..]
                .iter()
                .all(|page| page.cursor == 0),
            "inactive arena page must have zero cursor"
        );

        let mut report = ArenaPurgeReport::default();
        for page in &mut self.pages[self.active_pages..] {
            report.examined += 1;
            match page.backing_state() {
                PageBackingState::Discarded => report.already_discarded += 1,
                PageBackingState::Retained if !self.backend.supports_discard() => {
                    report.unsupported += 1;
                }
                PageBackingState::Retained => {
                    report.eligible += 1;
                    if page.backing.discard().is_ok() {
                        report.discarded += 1;
                    } else {
                        report.failed += 1;
                    }
                }
            }
        }
        #[cfg(feature = "aarm-telemetry")]
        self.refresh_telemetry_peaks();
        report
    }

    #[cfg(test)]
    fn discard_inactive_page_for_test(&mut self, index: usize) -> Result<*mut u8, ArenaAllocError> {
        assert!(
            index >= self.active_pages,
            "only inactive pages may be discarded"
        );
        let page = self
            .pages
            .get_mut(index)
            .expect("test discard references an existing page");
        assert_eq!(page.cursor, 0, "only zero-cursor pages may be discarded");
        let base = page.backing.base();
        page.backing.discard()?;
        #[cfg(feature = "aarm-telemetry")]
        self.refresh_telemetry_peaks();
        Ok(base)
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

        if let Some(pointer) = self.try_reuse_inactive_page(size, align)? {
            return Ok(pointer);
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

    fn try_reuse_inactive_page(
        &mut self,
        size: usize,
        align: usize,
    ) -> Result<Option<*mut u8>, ArenaAllocError> {
        let reusable_index = self.pages[self.active_pages..]
            .iter()
            .enumerate()
            .filter(|(_, page)| page.capacity() >= size)
            .min_by_key(|(_, page)| page.capacity())
            .map(|(offset, _)| self.active_pages + offset);
        let Some(index) = reusable_index else {
            return Ok(None);
        };

        self.pages[index].prepare_for_reuse()?;
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
        Ok(Some(ptr))
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
        let backing = self.backing_telemetry();
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
            virtual_extent_bytes: backing.virtual_extent,
            backing_retained_bytes: backing.retained,
            backing_discarded_bytes: backing.discarded,
            peak_backing_retained_bytes: self.telemetry.peak_backing_retained_bytes,
            events: self.telemetry.events,
            last_rewind: self.telemetry.last_rewind,
        })
    }

    #[cfg(feature = "aarm-telemetry")]
    fn refresh_telemetry_peaks(&mut self) {
        self.telemetry.peak_used_bytes = self.telemetry.peak_used_bytes.max(self.used_bytes);
        self.telemetry.peak_capacity_bytes =
            self.telemetry.peak_capacity_bytes.max(self.reserved_bytes);
        if let Some(retained_bytes) = self.backing_telemetry().retained {
            self.telemetry.peak_backing_retained_bytes = Some(
                self.telemetry
                    .peak_backing_retained_bytes
                    .unwrap_or(0)
                    .max(retained_bytes),
            );
        }
    }

    #[cfg(feature = "aarm-telemetry")]
    fn backing_telemetry(&self) -> BackingTelemetry {
        if !self.backend.has_known_virtual_extent() {
            return BackingTelemetry::default();
        }

        let Some(virtual_extent_bytes) = self
            .pages
            .iter()
            .map(Page::virtual_extent)
            .try_fold(0usize, |total, extent| total.checked_add(extent?))
        else {
            return BackingTelemetry::default();
        };

        let mut retained_bytes = 0usize;
        let mut discarded_bytes = 0usize;
        for page in &self.pages {
            let Some(extent) = page.virtual_extent() else {
                return BackingTelemetry::default();
            };
            let target = match page.backing_state() {
                PageBackingState::Retained => &mut retained_bytes,
                PageBackingState::Discarded => &mut discarded_bytes,
            };
            let Some(next) = target.checked_add(extent) else {
                return BackingTelemetry::default();
            };
            *target = next;
        }
        debug_assert_eq!(
            virtual_extent_bytes,
            retained_bytes + discarded_bytes,
            "whole-backing states partition the VM extent"
        );
        BackingTelemetry {
            virtual_extent: Some(virtual_extent_bytes),
            retained: Some(retained_bytes),
            discarded: Some(discarded_bytes),
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
    use std::sync::{Arc, atomic::AtomicUsize};

    struct CountingPageBackend {
        fail_allocations: bool,
        allocation_calls: AtomicUsize,
        release_calls: AtomicUsize,
    }

    #[cfg(feature = "aarm-telemetry")]
    struct CountingVmPageBackend {
        fail_discard: bool,
        fail_restore: bool,
        allocation_calls: AtomicUsize,
        release_calls: AtomicUsize,
        discard_calls: AtomicUsize,
        restore_calls: AtomicUsize,
    }

    struct RestoreFailingPageBackend;

    struct DiscardFailingPageBackend;

    #[cfg(feature = "aarm-telemetry")]
    struct AlternatingDiscardPageBackend {
        discard_calls: AtomicUsize,
    }

    impl PageBackend for RestoreFailingPageBackend {
        fn allocate_zeroed(
            &self,
            capacity: usize,
            alignment: usize,
        ) -> Result<PageBacking, ArenaAllocError> {
            let mut backing = SYSTEM_ALLOCATOR_PAGE_BACKEND.allocate_zeroed(capacity, alignment)?;
            backing.vm_allocation_size = Some(capacity);
            Ok(backing)
        }

        fn release(&self, backing: PageBacking) {
            SYSTEM_ALLOCATOR_PAGE_BACKEND.release(backing);
        }

        fn supports_discard(&self) -> bool {
            true
        }

        fn discard(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
            Ok(())
        }

        fn prepare_for_reuse(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
            Err(ArenaAllocError::OutOfMemory)
        }

        #[cfg(feature = "aarm-telemetry")]
        fn has_known_virtual_extent(&self) -> bool {
            true
        }
    }

    static RESTORE_FAILING_PAGE_BACKEND: RestoreFailingPageBackend = RestoreFailingPageBackend;

    impl PageBackend for DiscardFailingPageBackend {
        fn allocate_zeroed(
            &self,
            capacity: usize,
            alignment: usize,
        ) -> Result<PageBacking, ArenaAllocError> {
            let mut backing = SYSTEM_ALLOCATOR_PAGE_BACKEND.allocate_zeroed(capacity, alignment)?;
            backing.vm_allocation_size = Some(capacity);
            Ok(backing)
        }

        fn release(&self, backing: PageBacking) {
            SYSTEM_ALLOCATOR_PAGE_BACKEND.release(backing);
        }

        fn supports_discard(&self) -> bool {
            true
        }

        fn discard(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
            Err(ArenaAllocError::OutOfMemory)
        }

        #[cfg(feature = "aarm-telemetry")]
        fn has_known_virtual_extent(&self) -> bool {
            true
        }
    }

    static DISCARD_FAILING_PAGE_BACKEND: DiscardFailingPageBackend = DiscardFailingPageBackend;

    #[cfg(feature = "aarm-telemetry")]
    impl PageBackend for AlternatingDiscardPageBackend {
        fn allocate_zeroed(
            &self,
            capacity: usize,
            alignment: usize,
        ) -> Result<PageBacking, ArenaAllocError> {
            let mut backing = SYSTEM_ALLOCATOR_PAGE_BACKEND.allocate_zeroed(capacity, alignment)?;
            backing.vm_allocation_size = Some(capacity);
            Ok(backing)
        }

        fn release(&self, backing: PageBacking) {
            SYSTEM_ALLOCATOR_PAGE_BACKEND.release(backing);
        }

        fn supports_discard(&self) -> bool {
            true
        }

        fn discard(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
            let call = self.discard_calls.fetch_add(1, Ordering::Relaxed);
            if call % 2 == 0 {
                Ok(())
            } else {
                Err(ArenaAllocError::OutOfMemory)
            }
        }

        fn prepare_for_reuse(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
            Ok(())
        }

        #[cfg(feature = "aarm-telemetry")]
        fn has_known_virtual_extent(&self) -> bool {
            true
        }
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

    #[cfg(feature = "aarm-telemetry")]
    impl CountingVmPageBackend {
        fn new(fail_discard: bool, fail_restore: bool) -> Self {
            Self {
                fail_discard,
                fail_restore,
                allocation_calls: AtomicUsize::new(0),
                release_calls: AtomicUsize::new(0),
                discard_calls: AtomicUsize::new(0),
                restore_calls: AtomicUsize::new(0),
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

    #[cfg(feature = "aarm-telemetry")]
    impl PageBackend for CountingVmPageBackend {
        fn allocate_zeroed(
            &self,
            capacity: usize,
            alignment: usize,
        ) -> Result<PageBacking, ArenaAllocError> {
            self.allocation_calls.fetch_add(1, Ordering::Relaxed);
            let mut backing = SYSTEM_ALLOCATOR_PAGE_BACKEND.allocate_zeroed(capacity, alignment)?;
            backing.vm_allocation_size = Some(capacity);
            Ok(backing)
        }

        fn release(&self, backing: PageBacking) {
            self.release_calls.fetch_add(1, Ordering::Relaxed);
            SYSTEM_ALLOCATOR_PAGE_BACKEND.release(backing);
        }

        fn supports_discard(&self) -> bool {
            true
        }

        fn discard(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
            self.discard_calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_discard {
                Err(ArenaAllocError::OutOfMemory)
            } else {
                Ok(())
            }
        }

        fn prepare_for_reuse(&self, _backing: &mut PageBacking) -> Result<(), ArenaAllocError> {
            self.restore_calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_restore {
                Err(ArenaAllocError::OutOfMemory)
            } else {
                Ok(())
            }
        }

        #[cfg(feature = "aarm-telemetry")]
        fn has_known_virtual_extent(&self) -> bool {
            true
        }
    }

    fn test_backend(fail_allocations: bool) -> &'static CountingPageBackend {
        Box::leak(Box::new(CountingPageBackend::new(fail_allocations)))
    }

    #[cfg(feature = "aarm-telemetry")]
    fn alternating_discard_backend() -> &'static AlternatingDiscardPageBackend {
        Box::leak(Box::new(AlternatingDiscardPageBackend {
            discard_calls: AtomicUsize::new(0),
        }))
    }

    #[cfg(feature = "aarm-telemetry")]
    fn test_vm_backend(fail_discard: bool, fail_restore: bool) -> &'static CountingVmPageBackend {
        Box::leak(Box::new(CountingVmPageBackend::new(
            fail_discard,
            fail_restore,
        )))
    }

    #[test]
    fn discarded_inactive_page_restore_failure_is_atomic_and_keeps_governor_capacity() {
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE));
        let mut arena = PagedArena::with_backend_and_memory_governor(
            &RESTORE_FAILING_PAGE_BACKEND,
            Arc::clone(&governor),
        );
        let mark = arena.mark();
        let _pointer = arena
            .try_alloc(1, 1, MIN_PAGE_SIZE)
            .expect("fresh page allocation succeeds");
        arena.rewind(mark);
        assert_eq!(
            arena.purge_inactive_pages(),
            ArenaPurgeReport {
                examined: 1,
                eligible: 1,
                discarded: 1,
                ..ArenaPurgeReport::default()
            }
        );
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Discarded);

        let used_before = arena.used_bytes;
        let capacity_before = arena.reserved_bytes;
        let governor_before = governor.telemetry();
        for _ in 0..64 {
            assert_eq!(
                arena.try_alloc(1, 1, MIN_PAGE_SIZE),
                Err(ArenaAllocError::OutOfMemory)
            );
        }
        assert_eq!(arena.active_pages, 0);
        assert_eq!(arena.used_bytes, used_before);
        assert_eq!(arena.reserved_bytes, capacity_before);
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Discarded);
        assert_eq!(governor.telemetry(), governor_before);
    }

    #[test]
    fn failed_discard_keeps_inactive_backing_retained() {
        let mut arena = PagedArena::with_backend(&DISCARD_FAILING_PAGE_BACKEND);
        let mark = arena.mark();
        arena
            .try_alloc(1, 1, MIN_PAGE_SIZE)
            .expect("fresh page allocation succeeds");
        arena.rewind(mark);

        for _ in 0..64 {
            assert_eq!(
                arena.purge_inactive_pages(),
                ArenaPurgeReport {
                    examined: 1,
                    eligible: 1,
                    failed: 1,
                    ..ArenaPurgeReport::default()
                }
            );
        }
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Retained);
        assert_eq!(arena.metrics().reserved_bytes, MIN_PAGE_SIZE);
    }

    #[test]
    fn controlled_purge_skips_unsupported_backing_without_calling_discard() {
        let backend = test_backend(false);
        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE));
        let mut arena =
            PagedArena::with_backend_and_memory_governor(backend, Arc::clone(&governor));
        let mark = arena.mark();
        arena
            .try_alloc(1, 1, MIN_PAGE_SIZE)
            .expect("fresh page allocation succeeds");
        arena.rewind(mark);

        let before = governor.telemetry();
        assert_eq!(
            arena.purge_inactive_pages(),
            ArenaPurgeReport {
                examined: 1,
                unsupported: 1,
                ..ArenaPurgeReport::default()
            }
        );
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Retained);
        assert_eq!(backend.allocation_calls.load(Ordering::Relaxed), 1);
        assert_eq!(governor.telemetry(), before);
    }

    #[cfg(feature = "aarm-telemetry")]
    #[test]
    fn controlled_purge_continues_after_deterministic_discard_failures() {
        let backend = alternating_discard_backend();
        let total_capacity = MIN_PAGE_SIZE + (MIN_PAGE_SIZE * 2) + (MIN_PAGE_SIZE * 4);
        let governor = Arc::new(MemoryGovernor::new(total_capacity));
        let mut arena =
            PagedArena::with_backend_and_memory_governor(backend, Arc::clone(&governor));
        let mark = arena.mark();
        for size in [MIN_PAGE_SIZE, MIN_PAGE_SIZE * 2, MIN_PAGE_SIZE * 4] {
            arena
                .try_alloc(size, 1, total_capacity)
                .expect("fresh page allocation succeeds");
        }
        arena.rewind(mark);
        let governor_before = governor.telemetry();
        let report = arena.purge_inactive_pages();
        assert_eq!(report.examined, 3);
        assert_eq!(report.eligible, 3);
        assert_eq!(report.discarded, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(report.already_discarded, 0);
        assert_eq!(report.unsupported, 0);
        assert_eq!(
            arena
                .pages
                .iter()
                .map(Page::backing_state)
                .collect::<Vec<_>>(),
            [
                PageBackingState::Discarded,
                PageBackingState::Retained,
                PageBackingState::Discarded,
            ]
        );
        assert_eq!(governor.telemetry(), governor_before);
        let telemetry = arena.telemetry_snapshot().expect("telemetry is enabled");
        assert_eq!(telemetry.capacity_bytes, total_capacity);
        assert_eq!(telemetry.virtual_extent_bytes, Some(total_capacity));
        assert_eq!(telemetry.backing_retained_bytes, Some(MIN_PAGE_SIZE * 2));
        assert_eq!(
            telemetry.backing_discarded_bytes,
            Some(MIN_PAGE_SIZE + (MIN_PAGE_SIZE * 4))
        );

        assert_eq!(
            arena.purge_inactive_pages(),
            ArenaPurgeReport {
                examined: 3,
                eligible: 1,
                failed: 1,
                already_discarded: 2,
                ..ArenaPurgeReport::default()
            }
        );
    }

    #[cfg(all(feature = "aarm-telemetry", any(windows, target_os = "linux")))]
    #[test]
    fn controlled_purge_never_discards_active_or_partial_pages() {
        let mut arena = PagedArena::new();
        let first = arena.alloc(16, 1);
        // SAFETY: `first` belongs to the still-active first page.
        #[allow(unsafe_code)]
        unsafe {
            *first = 0x5A;
        }
        let mark = arena.mark();
        arena.alloc(MIN_PAGE_SIZE * 2, 1);
        arena.rewind(mark);

        let report = arena.purge_inactive_pages();
        assert_eq!(
            report,
            ArenaPurgeReport {
                examined: 1,
                eligible: 1,
                discarded: 1,
                ..ArenaPurgeReport::default()
            }
        );
        assert_eq!(arena.active_page_count(), 1);
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Retained);
        assert_eq!(arena.pages[1].backing_state(), PageBackingState::Discarded);
        // SAFETY: the active first page was not considered by the purge.
        #[allow(unsafe_code)]
        let active_byte = unsafe { *first };
        assert_eq!(active_byte, 0x5A);
    }

    #[cfg(feature = "aarm-telemetry")]
    #[test]
    fn backing_telemetry_marks_discarded_backing_without_changing_logical_capacity() {
        let mut arena = PagedArena::with_backend(&RESTORE_FAILING_PAGE_BACKEND);
        let mark = arena.mark();
        arena
            .try_alloc(1, 1, MIN_PAGE_SIZE)
            .expect("fresh page allocation succeeds");
        arena.rewind(mark);
        let retained = arena.telemetry_snapshot().expect("telemetry is enabled");
        assert_eq!(retained.capacity_bytes, MIN_PAGE_SIZE);
        assert_eq!(retained.virtual_extent_bytes, Some(MIN_PAGE_SIZE));
        assert_eq!(retained.backing_retained_bytes, Some(MIN_PAGE_SIZE));
        assert_eq!(retained.backing_discarded_bytes, Some(0));
        assert_eq!(retained.peak_backing_retained_bytes, Some(MIN_PAGE_SIZE));

        arena
            .discard_inactive_page_for_test(0)
            .expect("test backend discards inactive backing");
        let discarded = arena.telemetry_snapshot().expect("telemetry is enabled");
        assert_eq!(discarded.capacity_bytes, MIN_PAGE_SIZE);
        assert_eq!(discarded.virtual_extent_bytes, Some(MIN_PAGE_SIZE));
        assert_eq!(discarded.backing_retained_bytes, Some(0));
        assert_eq!(discarded.backing_discarded_bytes, Some(MIN_PAGE_SIZE));
        assert_eq!(discarded.peak_backing_retained_bytes, Some(MIN_PAGE_SIZE));
    }

    #[cfg(feature = "aarm-telemetry")]
    #[test]
    fn mixed_inactive_vm_pages_restore_in_best_fit_order_and_release_once() {
        let backend = test_vm_backend(false, false);
        let total_capacity = MIN_PAGE_SIZE + (MIN_PAGE_SIZE * 2) + (MIN_PAGE_SIZE * 4);
        let governor = Arc::new(MemoryGovernor::new(total_capacity));

        {
            let mut arena =
                PagedArena::with_backend_and_memory_governor(backend, Arc::clone(&governor));
            let mark = arena.mark();
            let small = arena
                .try_alloc(MIN_PAGE_SIZE, 1, total_capacity)
                .expect("small page allocation succeeds");
            let medium = arena
                .try_alloc(MIN_PAGE_SIZE * 2, 1, total_capacity)
                .expect("medium page allocation succeeds");
            let large = arena
                .try_alloc(MIN_PAGE_SIZE * 4, 1, total_capacity)
                .expect("large page allocation succeeds");
            arena.rewind(mark);
            assert_eq!(arena.page_count(), 3);

            assert_eq!(
                arena
                    .discard_inactive_page_for_test(0)
                    .expect("small inactive page discards"),
                small
            );
            assert_eq!(
                arena
                    .discard_inactive_page_for_test(2)
                    .expect("large inactive page discards"),
                large
            );
            let discarded = arena.telemetry_snapshot().expect("telemetry is enabled");
            assert_eq!(discarded.virtual_extent_bytes, Some(total_capacity));
            assert_eq!(discarded.backing_retained_bytes, Some(MIN_PAGE_SIZE * 2));
            assert_eq!(
                discarded.backing_discarded_bytes,
                Some(MIN_PAGE_SIZE + (MIN_PAGE_SIZE * 4))
            );

            let allocation_calls = backend.allocation_calls.load(Ordering::Relaxed);
            let grants = governor.telemetry().grant_events;
            let reuse_mark = arena.mark();
            let reused_small = arena
                .try_alloc(MIN_PAGE_SIZE, 1, total_capacity)
                .expect("smallest sufficient page restores first");
            let reused_medium = arena
                .try_alloc(MIN_PAGE_SIZE * 2, 1, total_capacity)
                .expect("retained medium page reuses directly");
            let reused_large = arena
                .try_alloc(MIN_PAGE_SIZE * 4, 1, total_capacity)
                .expect("discarded large page restores last");
            assert_eq!(reused_small, small);
            assert_eq!(reused_medium, medium);
            assert_eq!(reused_large, large);
            assert_eq!(
                backend.allocation_calls.load(Ordering::Relaxed),
                allocation_calls
            );
            assert_eq!(backend.restore_calls.load(Ordering::Relaxed), 2);
            assert_eq!(governor.telemetry().grant_events, grants);
            // SAFETY: every reused page was rewound before the test-only discard.
            #[allow(unsafe_code)]
            unsafe {
                assert!(
                    std::slice::from_raw_parts(reused_small, MIN_PAGE_SIZE)
                        .iter()
                        .all(|&byte| byte == 0)
                );
                assert!(
                    std::slice::from_raw_parts(reused_medium, MIN_PAGE_SIZE * 2)
                        .iter()
                        .all(|&byte| byte == 0)
                );
                assert!(
                    std::slice::from_raw_parts(reused_large, MIN_PAGE_SIZE * 4)
                        .iter()
                        .all(|&byte| byte == 0)
                );
            }

            arena.rewind(reuse_mark);
            arena
                .discard_inactive_page_for_test(1)
                .expect("mixed teardown keeps one discarded page");
            let mixed = arena.telemetry_snapshot().expect("telemetry is enabled");
            assert_eq!(mixed.virtual_extent_bytes, Some(total_capacity));
            assert_eq!(mixed.backing_retained_bytes, Some(MIN_PAGE_SIZE * 5));
            assert_eq!(mixed.backing_discarded_bytes, Some(MIN_PAGE_SIZE * 2));
            assert_eq!(mixed.peak_backing_retained_bytes, Some(total_capacity));
        }

        let telemetry = governor.telemetry();
        assert_eq!(backend.allocation_calls.load(Ordering::Relaxed), 3);
        assert_eq!(backend.release_calls.load(Ordering::Relaxed), 3);
        assert_eq!(telemetry.current_capacity_bytes, 0);
        assert_eq!(telemetry.grant_events, telemetry.release_events);
        assert_eq!(
            telemetry.granted_bytes_cumulative,
            telemetry.released_bytes_cumulative
        );
    }

    #[cfg(all(feature = "aarm-telemetry", any(windows, target_os = "linux")))]
    #[test]
    fn native_vm_backing_repeated_discard_reuse_preserves_accounting_and_address() {
        const TRANSITIONS: usize = 256;

        let governor = Arc::new(MemoryGovernor::new(MIN_PAGE_SIZE));
        let mut arena = PagedArena::with_memory_governor(Arc::clone(&governor));
        let mark = arena.mark();
        let base = arena
            .try_alloc(64, 8, MIN_PAGE_SIZE)
            .expect("fresh native VM page allocation succeeds");
        arena.rewind(mark);
        let initial = arena.telemetry_snapshot().expect("telemetry is enabled");
        let virtual_extent = initial
            .virtual_extent_bytes
            .expect("native VM backend reports an extent");
        assert_eq!(initial.backing_retained_bytes, Some(virtual_extent));
        assert_eq!(initial.backing_discarded_bytes, Some(0));

        for _ in 0..TRANSITIONS {
            assert_eq!(
                arena.purge_inactive_pages(),
                ArenaPurgeReport {
                    examined: 1,
                    eligible: 1,
                    discarded: 1,
                    ..ArenaPurgeReport::default()
                }
            );
            assert_eq!(arena.pages[0].backing.base(), base);
            assert_eq!(
                arena.purge_inactive_pages(),
                ArenaPurgeReport {
                    examined: 1,
                    already_discarded: 1,
                    ..ArenaPurgeReport::default()
                }
            );
            let discarded = arena.telemetry_snapshot().expect("telemetry is enabled");
            assert_eq!(discarded.virtual_extent_bytes, Some(virtual_extent));
            assert_eq!(discarded.backing_retained_bytes, Some(0));
            assert_eq!(discarded.backing_discarded_bytes, Some(virtual_extent));
            assert_eq!(discarded.peak_backing_retained_bytes, Some(virtual_extent));

            let reuse_mark = arena.mark();
            let reused = arena
                .try_alloc(64, 8, MIN_PAGE_SIZE)
                .expect("discarded native page restores before publication");
            assert_eq!(reused, base);
            // SAFETY: the restore path made the page retained before allocation.
            #[allow(unsafe_code)]
            let bytes = unsafe { std::slice::from_raw_parts(reused, 64) };
            assert!(bytes.iter().all(|&byte| byte == 0));
            let retained = arena.telemetry_snapshot().expect("telemetry is enabled");
            assert_eq!(retained.virtual_extent_bytes, Some(virtual_extent));
            assert_eq!(retained.backing_retained_bytes, Some(virtual_extent));
            assert_eq!(retained.backing_discarded_bytes, Some(0));
            assert_eq!(retained.peak_backing_retained_bytes, Some(virtual_extent));
            assert_eq!(
                governor.telemetry().current_capacity_bytes,
                MIN_PAGE_SIZE as u64
            );
            arena.rewind(reuse_mark);
        }

        let before_drop = governor.telemetry();
        assert_eq!(before_drop.current_capacity_bytes, MIN_PAGE_SIZE as u64);
        assert_eq!(before_drop.grant_events, 1);
        drop(arena);
        let after_drop = governor.telemetry();
        assert_eq!(after_drop.current_capacity_bytes, 0);
        assert_eq!(after_drop.grant_events, after_drop.release_events);
        assert_eq!(
            after_drop.granted_bytes_cumulative,
            after_drop.released_bytes_cumulative
        );
    }

    #[cfg(all(feature = "aarm-telemetry", any(windows, target_os = "linux")))]
    #[test]
    fn native_vm_regular_and_oversized_pages_preserve_exact_logical_capacity() {
        let page_sizes = [
            MIN_PAGE_SIZE,
            MIN_PAGE_SIZE * 2,
            MIN_PAGE_SIZE * 4,
            MIN_PAGE_SIZE * 8,
            DEFAULT_PAGE_SIZE,
            DEFAULT_PAGE_SIZE + 1,
            96 * 1024,
            (128 * 1024) + 7,
        ];
        let logical_capacity = page_sizes.iter().sum();
        let governor = Arc::new(MemoryGovernor::new(logical_capacity));
        let mut arena = PagedArena::with_memory_governor(Arc::clone(&governor));
        let mark = arena.mark();
        let bases = page_sizes
            .into_iter()
            .map(|size| {
                arena
                    .try_alloc(size, 1, logical_capacity)
                    .expect("native VM page allocation succeeds")
            })
            .collect::<Vec<_>>();
        arena.rewind(mark);

        let retained = arena.telemetry_snapshot().expect("telemetry is enabled");
        let virtual_extent = retained
            .virtual_extent_bytes
            .expect("native VM backend reports its extents");
        assert_eq!(retained.capacity_bytes, logical_capacity);
        assert!(virtual_extent >= logical_capacity);
        assert_eq!(retained.backing_retained_bytes, Some(virtual_extent));
        assert_eq!(retained.backing_discarded_bytes, Some(0));
        assert_eq!(
            governor.telemetry().current_capacity_bytes,
            logical_capacity as u64
        );

        assert_eq!(
            arena.purge_inactive_pages(),
            ArenaPurgeReport {
                examined: page_sizes.len(),
                eligible: page_sizes.len(),
                discarded: page_sizes.len(),
                ..ArenaPurgeReport::default()
            }
        );
        assert_eq!(
            arena
                .pages
                .iter()
                .map(|page| page.backing.base())
                .collect::<Vec<_>>(),
            bases
        );
        let discarded = arena.telemetry_snapshot().expect("telemetry is enabled");
        assert_eq!(discarded.capacity_bytes, logical_capacity);
        assert_eq!(discarded.virtual_extent_bytes, Some(virtual_extent));
        assert_eq!(discarded.backing_retained_bytes, Some(0));
        assert_eq!(discarded.backing_discarded_bytes, Some(virtual_extent));
        assert_eq!(discarded.peak_backing_retained_bytes, Some(virtual_extent));

        for (index, size) in page_sizes.into_iter().enumerate() {
            let reuse_mark = arena.mark();
            let reused = arena
                .try_alloc(size, 1, logical_capacity)
                .expect("discarded native page restores at its original address");
            assert_eq!(reused, bases[index]);
            // SAFETY: the page restored before reuse and `size` is its exact logical capacity.
            #[allow(unsafe_code)]
            let bytes = unsafe { std::slice::from_raw_parts(reused, size) };
            assert!(bytes.iter().all(|&byte| byte == 0));
            arena.rewind(reuse_mark);
        }

        let restored = arena.telemetry_snapshot().expect("telemetry is enabled");
        assert_eq!(restored.capacity_bytes, logical_capacity);
        assert_eq!(restored.virtual_extent_bytes, Some(virtual_extent));
        assert_eq!(restored.backing_retained_bytes, Some(virtual_extent));
        assert_eq!(restored.backing_discarded_bytes, Some(0));
        assert_eq!(restored.peak_backing_retained_bytes, Some(virtual_extent));
        let before_drop = governor.telemetry();
        assert_eq!(before_drop.grant_events, page_sizes.len() as u64);
        assert_eq!(before_drop.current_capacity_bytes, logical_capacity as u64);
        drop(arena);
        let after_drop = governor.telemetry();
        assert_eq!(after_drop.current_capacity_bytes, 0);
        assert_eq!(after_drop.grant_events, after_drop.release_events);
        assert_eq!(
            after_drop.granted_bytes_cumulative,
            after_drop.released_bytes_cumulative
        );
    }

    #[cfg(feature = "aarm-telemetry")]
    #[test]
    fn system_backing_reports_vm_capacity_as_unavailable() {
        let backend = test_backend(false);
        let mut arena = PagedArena::with_backend(backend);
        arena
            .try_alloc(1, 1, MIN_PAGE_SIZE)
            .expect("system-backed page allocation succeeds");

        let telemetry = arena.telemetry_snapshot().expect("telemetry is enabled");
        assert_eq!(telemetry.virtual_extent_bytes, None);
        assert_eq!(telemetry.backing_retained_bytes, None);
        assert_eq!(telemetry.backing_discarded_bytes, None);
        assert_eq!(telemetry.peak_backing_retained_bytes, None);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn retained_inactive_vm_page_reuse_keeps_its_backing_state() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();
        let pointer = arena
            .try_alloc(16, 1, MIN_PAGE_SIZE)
            .expect("fresh VM page allocation succeeds");
        arena.rewind(mark);
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Retained);

        let reused = arena
            .try_alloc(16, 1, MIN_PAGE_SIZE)
            .expect("retained inactive page reuses without restoration");
        assert_eq!(reused, pointer);
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Retained);
    }

    #[cfg(all(feature = "aarm-telemetry", any(windows, target_os = "linux")))]
    #[test]
    fn retained_inactive_vm_page_reuse_keeps_backing_telemetry() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();
        arena
            .try_alloc(16, 1, MIN_PAGE_SIZE)
            .expect("fresh VM page allocation succeeds");
        arena.rewind(mark);
        let before = arena.telemetry_snapshot().expect("telemetry is enabled");

        arena
            .try_alloc(16, 1, MIN_PAGE_SIZE)
            .expect("retained inactive page reuses without restoration");
        let after = arena.telemetry_snapshot().expect("telemetry is enabled");
        assert_eq!(after.virtual_extent_bytes, before.virtual_extent_bytes);
        assert_eq!(after.backing_retained_bytes, before.backing_retained_bytes);
        assert_eq!(after.backing_discarded_bytes, Some(0));
    }

    #[cfg(all(feature = "aarm-telemetry", any(windows, target_os = "linux")))]
    #[test]
    fn oversized_vm_page_keeps_exact_logical_capacity_and_rounded_extent() {
        let capacity = DEFAULT_PAGE_SIZE + 1;
        let mut arena = PagedArena::new();
        arena
            .try_alloc(capacity, 1, usize::MAX)
            .expect("oversized VM page allocation succeeds");

        let telemetry = arena.telemetry_snapshot().expect("telemetry is enabled");
        assert_eq!(telemetry.capacity_bytes, capacity);
        let virtual_extent = telemetry
            .virtual_extent_bytes
            .expect("VM backend reports its extent");
        assert!(virtual_extent >= capacity);
        assert_eq!(telemetry.backing_retained_bytes, Some(virtual_extent));
        assert_eq!(telemetry.backing_discarded_bytes, Some(0));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_default_backend_is_anonymous_memory_backed() {
        let arena = PagedArena::new();
        assert!(arena.backend.is_linux_anonymous());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_mapping_size_overflow_is_controlled() {
        assert_eq!(
            LinuxAnonymousPageBackend::mapping_size(usize::MAX),
            Err(ArenaAllocError::AddressSpace)
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_virtual_backing_decommits_recommits_at_its_original_zeroed_address() {
        let capacity = DEFAULT_PAGE_SIZE + 1;
        let backing = WINDOWS_VIRTUAL_PAGE_BACKEND
            .allocate_zeroed(capacity, MAX_ALIGN)
            .expect("Windows virtual allocation succeeds");
        assert_eq!(backing.capacity(), capacity);
        assert!(backing.vm_allocation_size.expect("Windows allocation size") >= capacity);
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_anonymous_backing_discards_to_demand_zero_without_moving() {
        let capacity = DEFAULT_PAGE_SIZE + 1;
        let backing = LINUX_ANONYMOUS_PAGE_BACKEND
            .allocate_zeroed(capacity, MAX_ALIGN)
            .expect("Linux anonymous mapping succeeds");
        let mapping_size = backing
            .vm_allocation_size
            .expect("Linux mapping size is recorded");
        let page_size = linux_page_size().expect("Linux page size is valid");
        assert_eq!(backing.capacity(), capacity);
        assert!(mapping_size >= capacity);
        assert_eq!(mapping_size % page_size, 0);
        assert_eq!(backing.base as usize % MAX_ALIGN, 0);
        let base = backing.base;
        // SAFETY: `backing` owns a writable anonymous mapping of `capacity` bytes.
        #[allow(unsafe_code)]
        unsafe {
            let bytes = std::slice::from_raw_parts_mut(backing.base, capacity);
            assert!(bytes.iter().all(|&byte| byte == 0));
            bytes.fill(0xA5);
        }

        LinuxAnonymousPageBackend::discard(&backing)
            .expect("MADV_DONTNEED keeps the anonymous mapping accessible");
        // SAFETY: MADV_DONTNEED preserves the mapping and future anonymous pages
        // fault back in as zero-filled memory at the same base.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(backing.base, capacity) };
        assert_eq!(backing.base, base);
        assert!(bytes.iter().all(|&byte| byte == 0));
        assert!(LinuxAnonymousPageBackend::release_backing(&backing));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_anonymous_backing_raii_drop_releases_its_mapping() {
        let mut backing =
            BackendPageBacking::allocate(&LINUX_ANONYMOUS_PAGE_BACKEND, MIN_PAGE_SIZE, MAX_ALIGN)
                .expect("Linux anonymous mapping succeeds");
        assert!(!backing.base().is_null());
        backing
            .discard()
            .expect("MADV_DONTNEED succeeds before release");
        drop(backing);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_discarded_inactive_page_restores_zeroed_at_its_original_base() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();
        let pointer = arena
            .try_alloc(16, 1, MIN_PAGE_SIZE)
            .expect("fresh Linux mapping succeeds");
        arena.rewind(mark);
        #[cfg(feature = "aarm-telemetry")]
        let virtual_extent = {
            let retained = arena.telemetry_snapshot().expect("telemetry is enabled");
            let virtual_extent = retained
                .virtual_extent_bytes
                .expect("Linux mapping extent is known");
            assert_eq!(retained.backing_retained_bytes, Some(virtual_extent));
            assert_eq!(retained.backing_discarded_bytes, Some(0));
            virtual_extent
        };
        // SAFETY: the page remains mapped after rewind and owned by the arena.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write_bytes(pointer, 0xA5, 16);
        }
        let base = arena
            .discard_inactive_page_for_test(0)
            .expect("MADV_DONTNEED succeeds");
        assert_eq!(base, pointer);
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Discarded);
        #[cfg(feature = "aarm-telemetry")]
        {
            let discarded = arena.telemetry_snapshot().expect("telemetry is enabled");
            assert_eq!(discarded.virtual_extent_bytes, Some(virtual_extent));
            assert_eq!(discarded.backing_retained_bytes, Some(0));
            assert_eq!(discarded.backing_discarded_bytes, Some(virtual_extent));
        }

        let reused = arena
            .try_alloc(16, 1, MIN_PAGE_SIZE)
            .expect("Linux mapping remains reusable");
        assert_eq!(reused, pointer);
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Retained);
        // SAFETY: Linux discard retains the mapping before it is reused.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(reused, 16) };
        assert!(bytes.iter().all(|&byte| byte == 0));
        #[cfg(feature = "aarm-telemetry")]
        {
            let retained = arena.telemetry_snapshot().expect("telemetry is enabled");
            assert_eq!(retained.virtual_extent_bytes, Some(virtual_extent));
            assert_eq!(retained.backing_retained_bytes, Some(virtual_extent));
            assert_eq!(retained.backing_discarded_bytes, Some(0));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_virtual_backing_raii_drop_releases_its_reservation() {
        let mut backing =
            BackendPageBacking::allocate(&WINDOWS_VIRTUAL_PAGE_BACKEND, MIN_PAGE_SIZE, MAX_ALIGN)
                .expect("Windows virtual allocation succeeds");
        let base = backing.base();
        backing.discard().expect("decommit succeeds before release");
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
    fn windows_discarded_inactive_page_restores_zeroed_at_its_original_base() {
        let mut arena = PagedArena::new();
        let mark = arena.mark();
        let pointer = arena
            .try_alloc(16, 1, MIN_PAGE_SIZE)
            .expect("fresh Windows page allocation succeeds");
        arena.rewind(mark);
        #[cfg(feature = "aarm-telemetry")]
        let virtual_extent = {
            let retained = arena.telemetry_snapshot().expect("telemetry is enabled");
            let virtual_extent = retained
                .virtual_extent_bytes
                .expect("Windows reservation extent is known");
            assert_eq!(retained.backing_retained_bytes, Some(virtual_extent));
            assert_eq!(retained.backing_discarded_bytes, Some(0));
            virtual_extent
        };
        // SAFETY: the page remains retained after rewind and owned by the arena.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write_bytes(pointer, 0xA5, 16);
        }
        let base = arena
            .discard_inactive_page_for_test(0)
            .expect("Windows decommit succeeds");
        assert_eq!(base, pointer);
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Discarded);
        #[cfg(feature = "aarm-telemetry")]
        {
            let discarded = arena.telemetry_snapshot().expect("telemetry is enabled");
            assert_eq!(discarded.virtual_extent_bytes, Some(virtual_extent));
            assert_eq!(discarded.backing_retained_bytes, Some(0));
            assert_eq!(discarded.backing_discarded_bytes, Some(virtual_extent));
        }

        let reused = arena
            .try_alloc(16, 1, MIN_PAGE_SIZE)
            .expect("Windows recommit succeeds");
        assert_eq!(reused, pointer);
        assert_eq!(arena.pages[0].backing_state(), PageBackingState::Retained);
        // SAFETY: reuse prepared the backing before it was activated.
        #[allow(unsafe_code)]
        let bytes = unsafe { std::slice::from_raw_parts(reused, 16) };
        assert!(bytes.iter().all(|&byte| byte == 0));
        #[cfg(feature = "aarm-telemetry")]
        {
            let retained = arena.telemetry_snapshot().expect("telemetry is enabled");
            assert_eq!(retained.virtual_extent_bytes, Some(virtual_extent));
            assert_eq!(retained.backing_retained_bytes, Some(virtual_extent));
            assert_eq!(retained.backing_discarded_bytes, Some(0));
        }
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_default_arena_preserves_exact_oversized_logical_capacity() {
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
