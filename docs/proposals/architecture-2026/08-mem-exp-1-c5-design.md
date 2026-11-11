# MEM-EXP-1 C5 — ownership pass, validator, and lowering (implementation contract)

Status: experiment design for T-MEM-EXP-1 stage C. Not a language feature. C0 stays the default.
Configurations: `mem-exp-c5` (naive counting, no elision) and later `mem-exp-c6` (elision), both in
`aster-codegen-cranelift`, both on top of `mem-exp-c4` (H12 + size-class heap).

This is the **first, conservative increment** of master plan §5.5. It is sound by leaking: wherever
full ownership (collection glue, enum drop glue, closure environments) is not implemented yet, a
reference stored there receives a count that is never released. Leaks are measured and reported;
they are never a use-after-free.

## 1. Vocabulary

- **Leaves of a type** `leaves(T)`: `Class` (except native-header classes, below) and `Interface` are one leaf (an interface's leaf is
  its object word). A struct (`Type::User` naming a struct) has the leaves of its fields, prefixed by
  the field. Every other type (`String`, `Array`, `List`, `Dictionary`, `Task`, `Enum`, scalars) has
  no leaves. A type is **reference-bearing** when it has at least one leaf.
- **Native-header classes**: the `StringBuilder` class (any class named by `AllocateStringBuilder`,
  `StringBuilderAppend`, or `StringBuilderToString`) has a runtime header whose offset 8 is its
  length, not a count word. It is never a leaf, and descriptors never list fields of that type.
- **Owning place**: `Local(_)`; `Field { base }` when `base` is owning; `ObjectField { .. }` (class
  fields are released through the class descriptor). **Non-owning place**: `Index`, `EnumField`,
  `Symbol`, and any `Field` whose base is non-owning. Counts stored into non-owning storage are never
  released (leak by design in C5).
- **Owned sources** (the value arrives with one count): `Call`, `CallInterface`, `ForeignCall`
  results, `AllocateObject`. Everything else that produces a reference is **borrowed**.

## 2. Ownership pass (`aster_mir::ownership::insert_ownership`)

Per function, on a clone of the module:

1. Remove `OwnedRegionEnter`/`OwnedRegionExit` and `TemporarySubregionEnter`/`TemporarySubregionExit`.
   A mid-function rewind leaves counted locals pointing into reclaimed memory; the exit release
   would then read it (found by `immediate_death_object_subregion_executes_without_a_stale_read`,
   which aborted on "release of an object whose count is already zero"). Removal only stops
   reclamation; function-level Temporary scopes still rewind after the exit releases.
2. **Entry**: create a new entry block that `Retain`s every leaf of every parameter, then `Goto`s the
   old entry. Parameters thereby become owned slots.
3. For every instruction `I`, emit in this order:
   1. **Consumed operands** (`Retain` before `I`): every leaf of every reference-bearing operand of
      `ListAdd.value`, `ListSet.value`, `DictionaryAdd`/`DictionarySet` key and value, every
      `CallIntrinsic` argument, every `ForeignCall` argument, `EnumConstruct` fields, and the source
      operands of an `Assign` whose target is non-owning.
   2. **Borrowed sources of an owning destination** (`Retain` before `I`): for `Assign` to an owning
      reference-bearing place, every leaf of the source operands (`Use`, `Aggregate` fields,
      `MakeInterface.object`, `Cast` operand).
   3. **Capture** for every owning reference-bearing destination `D` of `I`: for each leaf `l`, a
      fresh temporary local `T` (`temporary: true`) and `Assign T = Use(Copy(D.l))`.
   4. `I` itself.
   5. **Borrowed results written by the runtime** (`Retain(Copy(D.l))` after `I`): destinations of
      `ListGet`, `DictionaryTryGet`, and `CallIntrinsic`.
   6. `Release(Copy(T))` for every capture temporary of step 3.
4. **Return/End**: for `Return(Some(op))` with a reference-bearing `op`: fresh local `R`,
   `Assign R = Use(op)`, `Retain` every leaf of `R`; then `Release` every leaf of every parameter and
   local **except** pass temporaries and `R`; then `Return(Some(Copy(R)))`. `Return(None)`/`End`:
   release every parameter and non-temporary local leaf. Any other `Return(Some(Copy(..)))` is also
   materialized into `R` before the releases: a scalar such as `return this.value` reads through a
   reference (first draft of this rule was wrong; the validator caught it as U2).
5. Statistics: number of inserted `Retain` and `Release` instructions.

Call arguments and interface receivers are passed borrowed; the callee's entry retain (2) makes the
G1 guarantor question moot in C5 (no callee code runs before it). C6 replaces it with summaries.

## 3. Backend contract

- Every reference-bearing local and parameter stack slot is **zero-initialized** at function entry
  in C5, so releasing a never-assigned slot or capturing it is a no-op on null.
- `Retain(v)`/`Release(v)` load the object word (the class pointer, or offset 0 of an interface
  value) and call `aster_rt_object_retain(object)` / `aster_rt_object_release(context, object)`;
  both ignore null and `IMMORTAL`.
- Allocation writes the class **descriptor pointer** at offset 0 (H5); the runtime writes the count:
  1 for a Persistent object served by a size class, `IMMORTAL` for a Temporary object or one above
  2,048 bytes.
- Descriptors (`aster_runtime::ObjectDescriptor`: size and leaf offsets, including leaves of inline
  struct fields) are owned by the prepared program and outlive every object.
- Failure edges emit no release (balance rule exemption); the domain heap is released in bulk.

## 4. Validator (`aster_mir::validate::ownership::validate_ownership`)

Independent code. Symbolic token accounting per block, with a forward dataflow for slot states.

- **Slots**: owning local leaves `(LocalId, field path)`. State per slot: `Null` (entry, never
  written), `Live` (holds a value with one count), `Dead` (released). Join: equal states stay;
  `Null ⊔ Live = Live`; anything with `Dead` is `Dead`.
- **Values**: symbolic ids. Each id has `tokens` and a provenance: `Slot` (opaque value of a Live slot
  at block entry, 1 token), `Owned` (owned source, 1 token), `HeapRead` (load from an owning heap
  place, 0 tokens, borrowed), `Guaranteed` (parameter before its entry retain, or a load from
  non-owning storage, which is never released in C5), `Region` (Temporary allocation; exempt from
  token accounting).
- Heap reads and non-owning reads of a structurally equal place return the same id until an
  invalidating instruction:
  any `Release`, call of any kind, any heap or non-owning store, or a write to a local the place
  mentions.
- Effects: `Retain(v)` +1; `Release(v)` −1 and the operand slot becomes `Dead`; a store of `v` into
  an owning heap place −1 and the cached old value of that place +1 (adoption); a store into
  non-owning storage or a consumed operand −1; a local write moves the slot to the new value.
- Findings:
  - **U1** `Release` of a value with 0 tokens, or of a `Dead` slot (double release / not owned).
  - **U1** tokens below zero at any point.
  - **U1** at a block end (`Goto`/`Branch`): for some value, tokens ≠ number of `Live` slots holding
    it (leak or missing count).
  - **U1** at `Return`/`End`: a `Live` slot that is not the returned local, or tokens of the returned
    value ≠ 1.
  - **U1** a store into an owning heap place whose old value was not read in the same window.
  - **U2** use of a `HeapRead` value with 0 tokens after an invalidating instruction.
  - **U4** a `Region` value stored into a heap place whose base is known non-region.
- **U3** any region marker (`OwnedRegion*`, `TemporarySubregion*`) left in a function with
  reference-bearing locals. Otherwise U3 is the existing region validation; U5 holds because a region object's stored references carry
  a count that is never released; U6 holds because frame slots are consumed operands. U7 and U8 are
  runtime obligations, not claimed by C5.

## 5. Consequences measured so far

- C5 disables owned-region and fine-subregion reclamation. Twenty tests that measure region
  reclamation (peak or final used bytes under rewinds) are ignored under `mem-exp-c5` with that
  reason; their result checks still pass in C0 and C4. Recovering region reclamation together with
  counting requires the H2 pin rule (§5.4), not implemented in this increment.
- Released heap blocks return to size-class free lists, so `used_bytes` (arena accounting) does not
  fall when objects are released; reuse shows in `ObjectHeapStats.reused_blocks`. Live versus
  reserved memory must be reported from heap statistics, not from `used_bytes`.

## 6. C6 elision (contract)

`insert_ownership_elided` produces the C5 output and then removes count operations by three local
rules. Each rule keeps every value's token balance; none relies on a callee summary.

- **E1, null capture.** A capture `T = D.l` and its `Release(T)` are dropped when the destination
  slot is a local leaf that is definitely null at that point (never written on any path from the
  function entry). Releasing null is a no-op; the instruction pair only costs a call.
- **E2, move from a single-use temporary.** For a lowering temporary `t` (`temporary: true`,
  not created by the pass) that is written exactly once and whose only read is a copy into an
  owning destination later in the same block, the `Retain` of that copy and the exit `Release` of
  `t` are dropped: the count moves with the value.
- **E3, leaf-function parameters.** When a function, after C5 insertion, contains no `Release`
  other than parameter exit releases and no call of any kind, and never writes a parameter slot,
  the entry `Retain` and exit `Release` of every parameter are dropped. Nothing in such a function
  can release any object, so the caller's guarantee covers the whole call.

Validator changes for C6 (independent code, still no sharing with the pass):

- A slot counts as a holder at a block end only when it is live (read before any redefinition on
  some path, exit releases included). A dead slot's bits are never read again, so it needs no
  count.
- At `Return`/`End` the check is token balance only: every value has 0 tokens, except the returned
  value with 1. The rule "every Live slot was released" is dropped (it is implied for live slots).
- A parameter's original value may be held without a count (tokens = holders − 1 at block ends,
  0 at return unless returned) only when the function contains no `Release` of a non-parameter
  slot and no call of any kind; otherwise the C5 rule (tokens = holders) applies, and a borrowed
  parameter read after such an instruction is U2.

Metrics: static `Retain`/`Release` counts emitted by C5 and C6 for the same module, the number
removed by each rule, and executed counts from `ObjectHeapStats` (non-null, non-immortal).

## 7. `List<T>` element ownership (U8, first collection)

A `List<T>` whose element type has leaves **owns one count per leaf of each live element**. The
list header itself is still not counted: when the list dies, its elements leak (bounded by the
list's length). Arrays, dictionaries, and enums keep the leak rule of §1 for now.

- **Element descriptor.** Under C5/C6 the list header carries a pointer to an immutable
  `ObjectDescriptor` for the element type (`size` = element size, `reference_offsets` = leaf
  offsets inside one element; native-header classes excluded). Null when the element type has no
  leaves. Codegen sets it right after `AllocateList` (`aster_rt_list_set_element_descriptor`);
  every runtime path that creates a list from another list copies it.
- **Runtime glue** (no user code runs; the element's count operations use the list's context):
  - `ListAdd`, `ListInsert`: the caller transfers one count per leaf (the pass already retains the
    value as a consumed operand). Growth moves bits.
  - `ListSet`: store the new element, then release the old element's leaves (store sequence §4.B,
    steps 4 to 6 in the runtime).
  - `ListRemoveAt`, `ListRemoveRange`: restore length and shift first, then release the removed
    elements.
  - `ListClear`: set the length to zero first, then release every former element.
  - `ListToArray`, `ListGetRange`, `ListAddRange`: retain every leaf of every copied element (the
    copy is a new holder; an array copy then leaks under §1).
  - `ListReverse`, growth, and shifts: permutations; no count change.
- **Pass.** Unchanged: values read by `ListGet` are retained into their owning destination right
  after the read, and stored values are retained before the store.
- **Validator.** A value read from a list element is borrowed, not guaranteed: it is valid only
  until the next invalidating instruction, which now also includes `ListSet`, `ListRemoveAt`,
  `ListClear`, and every intrinsic call. **E3** must treat `ListSet`, `ListRemoveAt`, and
  `ListClear` as releasing instructions (a function containing one does not qualify).

## 8. Counted arrays (U8, second collection)

Arrays become counted objects with the uniform header (B1 U6): under `mem-exp-rc` the
`AsterArray` header is `{ descriptor word @0, count word @8, length, element_size, data }`;
codegen keeps reading `length`/`data` through `ASTER_ARRAY_*_OFFSET`, so it follows the new layout.

- **Kind flag.** One of the three reserved count-word bits marks an array. The release work list
  checks it: for an array, the descriptor word points at the **element** descriptor (or is null),
  every leaf of every element is pushed, then the data block and the header block go back to the
  heap (a block above 2,048 bytes is not returned: it stays reserved, and is reported as such).
- **Count initialization.** Every array the runtime creates (`aster_rt_array_new`, copies, string
  and dictionary snapshots) starts at 1 when its header is served by a size class and Persistent,
  otherwise `IMMORTAL` (Temporary arrays).
- **Arrays own their elements.** The element descriptor is set by codegen right after
  `AllocateArray` and after every instruction that produces an array whose elements have leaves
  and whose producer did not already retain them (`DictionaryKeys`, `DictionaryValues`,
  `DictionaryEntries`): `aster_rt_array_adopt_elements(array, descriptor)` sets the descriptor
  and retains every leaf. `ListToArray` and `ListGetRange` set it from the list's descriptor and
  retain (they already retain).
- **Pass.** `Type::Array(_)` is a leaf (whatever its element type: an array of `int` must still be
  released to free its blocks). `Index` places are owning (store sequence with capture and release).
  Fresh producers are owned sources: `AllocateArray`, `ListToArray`, `DictionaryKeys`,
  `DictionaryValues`, `DictionaryEntries`, and every `CallIntrinsic` whose result type is an array.
- **Validator.** `Index` places are owning heap places (cache, adoption, U2 window); array reads are
  borrowed, no longer guaranteed. Array element stores invalidate. E3 treats `Index` stores as
  releasing (they release the old element).

## 9. Counted strings

Strings are not worker-transferable (`Primitive::is_worker_transferable` requires a bit width), so
H4 holds and their counts stay single-domain.

- **Header.** Under `mem-exp-rc`, `AsterStrHeader` gets the uniform prefix: descriptor word @0
  (always null for strings), count word @8 with a `STRING_FLAG` reserved bit, then `len`, then the
  UTF-8 payload. Every reader goes through the struct or exported offset constants; `encode_str`
  and codegen string constants emit the same layout with count `IMMORTAL | STRING_FLAG`.
- **Every string header is created through one allocation path** that writes the count: 1 for a
  Persistent string whose block is served by a size class, `IMMORTAL` for Temporary strings and
  static or constant strings. Any other construction site is a defect.
- **Release** of a `STRING_FLAG` object returns its block to the heap (no children).
- **Owned results.** Every runtime entry point that returns a string returns it owned: a fresh
  string has count 1; an entry point that returns one of its arguments (or another existing
  string) retains it first.
- **Pass and validator.** `Type::String` is a leaf. Owned sources add `StringBuilderToString` and
  every `CallIntrinsic` whose result type is `String` (as for arrays). `CallIntrinsic` arguments are
  **borrowed** (no retain) except for intrinsics that store an argument: `ListInsert` (element),
  `DictionaryGetOrAdd` (key and value), `AsyncStoreSlot` (value). The runtime never keeps a
  borrowed argument past the call and runs no user code; list and dictionary glue never releases an
  argument of the same call. String constants are immortal: a constant operand needs no count.
- **Element descriptors.** String leaves appear in class, struct, list-element, and array-element
  descriptors. Arrays the runtime builds with string elements (split, command-line arguments) get a
  runtime-owned string element descriptor.

## 10. Counted `List<T>` headers

- **Kind field.** The three reserved count-word bits become one 3-bit kind: 0 class object,
  1 array, 2 string, 3 list, 4 dictionary (reserved for §11). Every flag test becomes a kind test.
- **Header.** Under `mem-exp-rc`, `AsterList` starts with the uniform prefix: element descriptor
  @0 (moved from the trailing field of §7), count word @8 with kind 3, then the existing fields.
  Codegen reaches list fields only through runtime calls, so nothing else moves.
- **Blocks.** A Persistent list header comes from a size class (count 1); a Temporary list is
  `IMMORTAL`. Element buffers up to 2,048 bytes come from the size-class heap; growth returns the
  old buffer to the heap after copying (bit move, no count change). Larger buffers stay reserved
  and are reported.
- **Release** of a list: release every leaf of every live element (by the element descriptor),
  return the buffer, then the header.
- **Pass and validator.** `Type::List(_)` is a leaf. `AllocateList` is an owned source. Every
  intrinsic or instruction that receives a list operand borrows it. A list stored into a class
  field, struct, array, or another list follows the existing store rules.

## 11. Counted dictionaries

- **Header.** Under `mem-exp-rc`, `AsterDictionary` starts with the uniform prefix: descriptor word
  @0 pointing at an immutable `{ key, value }` pair of element descriptors (each may be null), count
  word @8 with kind 4. Codegen sets the pair right after `AllocateDictionary` when the key or value
  type has leaves. Persistent header, bucket, and entry blocks up to 2,048 bytes are heap-served;
  growth returns old blocks; Temporary dictionaries are `IMMORTAL`.
- **The dictionary owns the key and value of every live entry.** The caller transfers one count per
  leaf of the key and the value it passes to `Add`, `Set`, and `GetOrAdd` (the pass already treats
  them as consumed operands). The runtime then:
  - `Add` on a new key, and `Set`/`GetOrAdd` inserting: keeps both counts.
  - `Add` that does not insert (existing key or failure): releases the passed key and value.
  - `Set` on an existing key: stores the new value, releases the old value, releases the passed key
    (the entry keeps its original key).
  - `GetOrAdd` on an existing key: releases the passed key and value.
  - `Remove`: restores the table invariants first, then releases the removed key and value.
  - `Clear`: empties the table first, then releases every former key and value.
  - Drop (release of the dictionary): releases every live key and value, then returns the blocks.
  - `Keys`/`Values`/`Entries` snapshots: unchanged (the produced array adopts and retains).
- **Pass and validator.** `Type::Dictionary(_, _)` is a leaf. `AllocateDictionary` is an owned
  source. Dictionary operands are borrowed. Values read by `DictionaryTryGet`, `DictionaryGetOr`,
  and `DictionaryGetOrAdd` are borrowed until the next invalidating instruction, which now includes
  `DictionaryAdd`, `DictionarySet`, `DictionaryRemove`, and `DictionaryClear`. E3 does not apply to
  functions containing any of these four instructions.

## 12. Large block classes

The size-class heap gains power-of-two classes above 2,048 bytes: 4 KiB, 8 KiB, ... up to 1 MiB.
They serve **buffers only** (array data, list buffers, dictionary storage) and strings; class
objects and collection headers stay within the small classes. A large block is carved from the
Persistent arena on first use and returned to its class's free list on release or growth, exactly
like small blocks (zeroed on reuse, poisoned on release in debug builds). Blocks above 1 MiB stay
reserved and are reported. Rounding up to a power of two wastes at most half a large block; the
peak-memory gate of §4.C measures that cost.
