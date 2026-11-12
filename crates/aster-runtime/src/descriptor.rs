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

/// [`ClassDescriptor::flags`] bit: the class is abstract. No object header
/// ever points at the descriptor of an abstract class.
pub const CLASS_FLAG_ABSTRACT: u32 = 1;
/// [`ClassDescriptor::flags`] bit: no class derives from this class.
pub const CLASS_FLAG_SEALED: u32 = 1 << 1;

/// Immutable class descriptor: the target of the header word of every
/// instance of one class, and the carrier of its place in the (single
/// inheritance) hierarchy.
#[repr(C)]
pub struct ClassDescriptor {
    /// Size and reference layout; must stay the first field.
    pub object: ObjectDescriptor,
    /// Compiler identity of the class type (`aster_mir::type_key` of
    /// `Type::Class(symbol)`), distinct for distinct specializations.
    pub class_id: u64,
    /// Number of entries behind `interfaces`.
    pub interface_count: u32,
    /// `CLASS_FLAG_*` bits.
    pub flags: u32,
    /// Immutable array of `interface_count` entries (null when zero).
    pub interfaces: *const InterfaceEntry,
    /// Descriptor of the direct base class; null for a class without a base.
    pub base: *const ClassDescriptor,
    /// Number of ancestors: `0` for a class without a base, otherwise the
    /// base's depth plus one.
    pub depth: u32,
    /// Number of entries behind `ancestors`; always `depth + 1`.
    pub ancestor_count: u32,
    /// `depth + 1` descriptors, root class first, ending with this class
    /// itself: `ancestors[i]` is the ancestor at depth `i`. This is what makes
    /// the subtype test bounded (see [`ClassDescriptor::derives_from`]).
    pub ancestors: *const *const ClassDescriptor,
    /// Virtual table: one function address per slot (object model B1, section
    /// 4.2). Slot `i` holds the implementation that a virtual call of slot `i`
    /// runs for an object of exactly this class; an abstract slot holds a stub
    /// that records a controlled runtime error. Null when `vtable_count` is
    /// zero. Filled once, after the JIT module is finalized.
    pub vtable: *const *const u8,
    /// Number of entries behind `vtable`.
    pub vtable_count: u32,
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

    /// The virtual table of this class.
    ///
    /// # Safety
    ///
    /// `vtable` must point at `vtable_count` valid pointers that outlive the
    /// returned slice (true for every descriptor produced by the compiler
    /// backend while its program is alive).
    #[allow(unsafe_code)]
    #[must_use]
    pub unsafe fn vtable_entries(&self) -> &[*const u8] {
        if self.vtable.is_null() || self.vtable_count == 0 {
            return &[];
        }
        // SAFETY: guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(self.vtable, self.vtable_count as usize) }
    }

    /// The ancestor descriptors, root first, ending with this class.
    ///
    /// # Safety
    ///
    /// `ancestors` must point at `ancestor_count` valid pointers that outlive
    /// the returned slice (true for every descriptor produced by the compiler
    /// backend while its program is alive).
    #[allow(unsafe_code)]
    #[must_use]
    pub unsafe fn ancestor_entries(&self) -> &[*const ClassDescriptor] {
        if self.ancestors.is_null() || self.ancestor_count == 0 {
            return &[];
        }
        // SAFETY: guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(self.ancestors, self.ancestor_count as usize) }
    }

    /// The bounded subtype test of the object model (B1, section 4.5):
    /// whether `self` is `target` or derives from it. `target.depth <=
    /// self.depth` is checked first, so an unrelated shallow class tested
    /// against a deeper target never indexes past the `ancestors` array.
    ///
    /// # Safety
    ///
    /// Both descriptors must be valid compiler-produced descriptors (see
    /// [`Self::ancestor_entries`]).
    #[allow(unsafe_code)]
    #[must_use]
    pub unsafe fn derives_from(&self, target: &ClassDescriptor) -> bool {
        if target.depth > self.depth {
            return false;
        }
        // SAFETY: guaranteed by the caller.
        let ancestors = unsafe { self.ancestor_entries() };
        ancestors
            .get(target.depth as usize)
            .is_some_and(|ancestor| std::ptr::eq(*ancestor, target))
    }

    /// Whether the class is declared abstract.
    #[must_use]
    pub const fn is_abstract(&self) -> bool {
        self.flags & CLASS_FLAG_ABSTRACT != 0
    }

    /// Whether the class is declared sealed.
    #[must_use]
    pub const fn is_sealed(&self) -> bool {
        self.flags & CLASS_FLAG_SEALED != 0
    }
}
