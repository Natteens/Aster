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
