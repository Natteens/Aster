//! Immutable per-class metadata referenced by the first word of every class
//! object (object model B1, sections 2 to 4).
//!
//! Every class instance starts with an 8-byte header whose offset-0 word holds
//! a pointer to a [`ClassDescriptor`]. The descriptor never changes after the
//! program is prepared and outlives every object that points at it.

/// Leading part of every descriptor. The MEM-EXP-1 counting experiment reads
/// offset 0 of an object as `*const ObjectDescriptor`, so [`ClassDescriptor`]
/// keeps this struct as its first field.
#[repr(C)]
pub struct ObjectDescriptor {
    /// Allocation size of the object, header included.
    pub size: u32,
    /// Number of entries in `reference_offsets`. Zero unless reference
    /// counting is compiled in.
    pub reference_count: u32,
    /// Byte offsets of the fields that hold strong class references. Null
    /// unless reference counting is compiled in.
    pub reference_offsets: *const u32,
}

// SAFETY: a descriptor and its offset table are immutable after creation
// (module data, or `static` in tests) and are only ever read, so sharing a
// reference across threads cannot race.
#[allow(unsafe_code)]
unsafe impl Sync for ObjectDescriptor {}

/// One `(interface, method table)` pair of a [`ClassDescriptor`].
#[repr(C)]
pub struct InterfaceEntry {
    /// Compiler identity of the interface type (`aster_mir::type_key`).
    pub interface_id: u64,
    /// Address of the interface method table of this class, in finalized JIT
    /// data; the same table interface values of this class carry.
    pub table: *const u8,
}

// SAFETY: entries are filled once before any code runs and only read after.
#[allow(unsafe_code)]
unsafe impl Sync for InterfaceEntry {}

/// Immutable class descriptor: the target of the header word of every
/// instance of one class (no inheritance yet).
#[repr(C)]
pub struct ClassDescriptor {
    /// Size and reference layout; must stay the first field.
    pub object: ObjectDescriptor,
    /// Compiler identity of the class type (`aster_mir::type_key` of
    /// `Type::Class(symbol)`), distinct for distinct specializations.
    pub class_id: u64,
    /// Number of entries behind `interfaces`.
    pub interface_count: u32,
    /// Explicit padding so the layout is identical on every target.
    pub reserved: u32,
    /// Immutable array of `interface_count` entries (null when zero).
    pub interfaces: *const InterfaceEntry,
}

// SAFETY: a class descriptor is immutable once the program is prepared.
#[allow(unsafe_code)]
unsafe impl Sync for ClassDescriptor {}

impl ClassDescriptor {
    /// Interface entries of this class.
    ///
    /// # Safety
    ///
    /// `interfaces` must point at `interface_count` valid, initialized entries
    /// that outlive the returned slice (true for every descriptor produced by
    /// the compiler backend while its program is alive).
    #[allow(unsafe_code)]
    #[must_use]
    pub unsafe fn interface_entries(&self) -> &[InterfaceEntry] {
        if self.interfaces.is_null() || self.interface_count == 0 {
            return &[];
        }
        // SAFETY: guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(self.interfaces, self.interface_count as usize) }
    }
}
