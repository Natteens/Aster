# Architectural proposal: Object model B1 (single class inheritance)

**Status.** Direction B is **ACCEPTED as the objective**: syntax and ergonomics close to C#, a native
compiler and runtime independent of .NET, and no traditional GC (nominal single class inheritance with
polymorphism is how this proposal pursues it). The complete memory model (ownership, borrowing and the
reclamation of shared object graphs) is still pending and is a separate decision, but it must respect
this direction. B1 introduces no GC and keeps the current per-invocation arena model. Every B1 contract in this document is **PROPOSED**. Nothing here is implemented, and no public documentation may describe inheritance as
available until the pull request that implements a part of it lands. Section 11 separates design
acceptance, experimental implementation, performance validation and the authorization of the later
stages, so that no stage depends on a result that only a later stage can produce.

| | |
| --- | --- |
| Baseline | `c3ef946`, release 0.71.0 |
| Related | [classes](../reference/classes.md), [interfaces](../reference/interfaces.md), [generic types](../reference/generic-types.md), [monomorphization](monomorphization.md), [runtime ABI](runtime-abi.md), [execution context](execution-context.md), [visibility](../specification/15-visibility.md) |
| Supersedes (on acceptance) | the "class inheritance is not adopted" statements listed in section 12 |

## 1. Scope

B1 adds single implementation inheritance, `virtual`/`override`/`abstract`/`sealed`, `base(...)`
construction, `protected`, and class-to-base and class-to-interface conversions that respect the
dynamic type. It does not add `object` as a universal root, boxing, reflection, attributes, static
fields, delegates, events, finalizers, multiple implementation inheritance, or .NET binary
compatibility. Each of those needs its own record. B1 also leaves the arena model as it is. The absence of a
traditional GC is part of the accepted direction, not a limit specific to B1; what remains pending is
the complete ownership, borrowing and shared-graph reclamation model, which must stay within that
direction and is not decided here.

## 2. Approved decisions

These were approved by the maintainer and are inputs to this proposal.

1. **Uniform header.** Every class instance has a header, with a pointer to its dynamic descriptor at
   offset zero.
2. **Immutable descriptors.** One descriptor per specific class or generic specialization (here
   "specific" means closed and fully specialized, not "non-abstract"; abstract classes have one too,
   although no object header ever points to it). It carries the hierarchy, the virtual dispatch table
   and the interface tables.
3. **Constructors split.** B1 keeps one constructor per class and adds `base(...)`. B2 (separate
   record) adds constructor overloads and `this(...)`.
4. **Closed, fully specialized bases only.** A B1 base class is a closed, fully specialized type
   (section 8): a non-generic class, or a generic class whose type arguments are all concrete types. The
   base may be abstract. Inheriting from a base that mentions open type parameters is a later expansion
   that this design must not block.
5. **Identity-preserving upcast.** Upcasting never copies. Dispatch and conversions follow the dynamic
   type.

## 3. Facts about the current implementation that shape the design

Verified in the repository at the baseline:

- Objects before stage 2 were headerless. `aster_rt_object_new(context, size)` returns zeroed storage
  owned by the `ExecutionContext`. Stage 2 (implemented) gives every class instance an 8-byte header
  whose offset-0 word points at an immutable class descriptor. Pull request 1 (T-OBJ-2, single
  inheritance) is implemented: see the delivery note below section 11.
- `mir::Type::Class(SymbolId)` is an exact type. MIR has no subtype relation.
- Class layout is computed as a struct: fields in declaration order with natural alignment
  (`layouts.rs`), after the 8-byte header; a class with no fields occupies exactly the header (8 bytes).
- An interface value is two pointers: the object and a table. `MakeInterface { object, class,
  interface }` selects the table at compile time from the pair `(class, interface)`, and
  `CallInterface` calls through it.
- Constructors: exactly one per class (`classes.md`). The implicit constructor is synthesized only when
  every field has an initializer or zero-defaults.
- A constructor may use properties of `this`; the documented example `Player(int damage) { Health -=
  damage; }` does.
- Class references are not transferable to workers (`concurrency.md`).
- Escape analysis reports `InterfaceConversion` and `PassedToInterfaceCall`; local object elimination
  matches the exact `Type::Class(class)`; reclamation is per invocation.
- `base`, `virtual`, `override`, `abstract` and `sealed` are not tokens today. `protected` is a token
  and a visibility value, but no program can use it meaningfully.

## 4. Object layout and the descriptor

### 4.1 Header and offsets

`P` is the pointer size of the target (8 on a 64-bit host). `H` is the header size, a named
constant of the layout code. **`H = P`** (the H8 header on a 64-bit host): closure Q-OBJ-1 reserved
a count word after the descriptor pointer (H12 or H16) only if reference counting were adopted, and
MEM-EXP-1 rejected counting ([report](../proposals/architecture-2026/09-mem-exp-1-report.md)).
Layout is computed with checked 32-bit arithmetic; an overflow is a controlled error. The rules, for
a class `C` with optional base `B`:

1. **Header.** Offset `0` holds the descriptor pointer: `H` bytes, aligned to `P`.
2. **Start of the fields.** For a class with no base, the first field is placed at `align_up(H, field
   alignment)`. For a class with a base, the first own field is placed at `align_up(size(B), field
   alignment)`, where `size(B)` is the **complete** size of `B` including its tail padding. A derived
   field never occupies the tail padding of a base: the derived layout begins where the complete base
   object ends.
3. **Own fields.** Each own field is placed in declaration order at `align_up(running offset, its
   alignment)` and advances the running offset by its size, exactly as the layout code does for a class
   today.
4. **Alignment.** `align(C) = max(P, align(B), the alignment of every own field)`.
5. **Size.** `size(C) = align_up(end of the last field, align(C))`.
6. **Empty classes.** A class with no fields and no base has `size = H` and `align = P`; the header is
   all of it. This replaces the current one-byte size of a field-less class. A derived class with no own
   fields has `size(D) = size(B)` and `align(D) = align(B)`.
7. **Preservation.** Every field inherited from `B` keeps in `D` exactly the offset and size it has in
   `B`. Field symbols are shared, so code compiled against `B` is valid on any `D`. This is a property
   that MIR validation recomputes from the class definitions and rejects if it does not hold; it is not
   left to the backend.
8. **Upcast.** The same pointer. No code runs.

Worked example on a 64-bit host:

| Class | Fields | Offsets | `size` / `align` |
| --- | --- | --- | --- |
| `Empty` | none | header at 0 | 8 / 8 |
| `Base` | `long x; byte b` | `x` at 8, `b` at 16 | 24 / 8 (end 17, rounded up) |
| `Derived : Base` | `byte d` | `x` at 8, `b` at 16, `d` at 24 | 32 / 8 (not 17: the tail padding of `Base` is not reused) |
| `Leaf : Derived` | none | as `Derived` | 32 / 8 |
| `Counter` | `int score` | `score` at 8 | 16 / 8 (today 4) |

The descriptor pointer is written at allocation time, immediately after `aster_rt_object_new`, with the
descriptor of the class being instantiated (necessarily non-abstract). It is valid from the first
instruction after allocation, including in an invocation that later records a controlled runtime
error. No generated code other than the allocation sequence stores to offset `0`; MIR validation
rejects any other store there, so the header is never user-writable.

The layout is an internal ASTER contract, not a C or C# ABI.

### 4.2 Descriptor contents (PROPOSED shape, not a binary format)

| Field | Purpose |
| --- | --- |
| class identity | unique per class or specialization; `Container<int>` and `Container<string>` differ |
| base descriptor, depth, ancestor array | `depth` is the number of ancestors; `ancestors` has `depth + 1` entries and ends with the class itself; used by the bounds-safe subtype test of 4.5 |
| flags | abstract, sealed |
| instance size | for diagnostics and validation |
| virtual table | one slot per introduced virtual method; overrides reuse the slot; an abstract slot holds a stub that records a controlled runtime error |
| interface tables | a count and, for each implemented interface (declared or inherited), its identity and the table of concrete methods |

### 4.3 Lifetime

A descriptor is immutable data. It is written once when the JIT module is built, before any code of the
module runs, and nothing writes to it during execution. It is retained by the JIT module, the same way
interface tables are today, and it is released with the module. An object or interface value that
points to a descriptor belongs to an invocation that completes before the module is released; this is
the same invariant interface tables already rely on. Class references do not cross workers, so
descriptor identity is only compared inside one module, and a watch rebuild creates a new module and
never mixes descriptors. The acceptance criteria require confirming that no task or worker can still
run module code after the module is released.

### 4.4 Cost

Every object grows by one pointer, and by alignment: the table in 4.1 shows a one-`int` class growing
from 4 to 16 bytes. This affects allocation sizes in arena accounting, temporary-subregion size
estimates, and any test or benchmark that pins an object size. The cost is measured in stage 3 of
section 11 and the decision to continue depends on it; the measurement is not a precondition of the
experimental implementation that produces it.

### 4.5 Bounds-safe dynamic checks

Dynamic type information is only useful if reading it cannot read out of bounds, so every check below
is bounded by data stored in the descriptor and the descriptors themselves are validated.

- **Subtype test** for a target class `T` against an object whose descriptor is `D`: `T.depth <=
  D.depth && D.ancestors[T.depth] == T`. The comparison against `depth` happens first, so an unrelated
  shallow class tested against a deeper target never indexes past the array.
- **Descriptor validation when the module is built:** the base chain is acyclic; `depth` equals the
  base's `depth + 1`; `ancestors` has exactly `depth + 1` entries, begins with the base's entries and
  ends with the class itself; the virtual table has at least as many slots as the base's, and every
  inherited slot is either the base's entry or an override with an exactly matching signature; an
  abstract slot of a non-abstract class is an error; the interface list has no duplicate interface.
- **Interface table lookup** (for a conversion from a non-sealed class type): the search is bounded by
  the descriptor's interface count, or by the count when a fixed index is used (U3), and it compares
  interface identities. The static conversion rule guarantees the interface is present, and the module
  build verifies that every non-abstract class has a table, with exactly the interface's method count,
  for every interface its static type system says it implements. If a lookup nevertheless finds no
  table, the generated code records a controlled runtime error and does not read or call through
  anything; it never reads past the count and never dereferences a null table.
- **Virtual slot index** at a call site is a compile-time constant validated against the vtable length
  of the call's static type; every descriptor derived from it has at least that many slots by the
  validation above.

## 5. Inheritance and polymorphism

- A class has at most one base class and any number of interfaces. A single list after `:` names them;
  semantic analysis classifies each name as class or interface. A class base, if present, must be
  first. Structs, enums, interfaces and static classes cannot be bases. Circular inheritance is an error.
- Inherited members that are accessible are members of the derived class. A derived class does not
  duplicate base storage.
- An abstract class can be inherited, and normally is. It cannot be instantiated, but it has fields,
  a constructor that derived classes reach through `base(...)`, a layout and a descriptor, and it may
  declare abstract members.
- Methods are non-virtual by default. `virtual` introduces a slot. `override` replaces an inherited
  virtual or abstract method and needs an exactly matching signature (parameters and return type) and
  the same visibility. `abstract` declares a method without a body and makes the class abstract;
  an abstract class cannot be instantiated; a concrete class must override every inherited abstract
  member. `sealed` on a class forbids derivation.
- A method with the signature of an inherited method that does not use `override` is an error: no
  silent hiding and no `new` modifier in B1.
- Overload resolution collects the candidates of the static type and its bases; an override replaces
  the declaration it overrides and is not a separate candidate. Unlike C#, a derived-class candidate
  has no priority: the existing exact-match-then-safe-conversion rule decides, and ties are an error.
- A call on a virtual method loads the descriptor, loads the slot, and calls the function with `this`
  first, using the ordinary call ABI. A non-virtual call is resolved statically.
- `base.Method(...)` is a direct call to the base class's implementation with no redispatch.
- Static methods are not virtual. Properties are methods in the semantic model; B1 allows `virtual`,
  `override` and `abstract` on methods only (see U9).
- `protected` members are accessible inside the declaring class and its derived classes, and only
  through a receiver whose static type is the accessing class or a class derived from it. A
  `Base`-typed receiver does not grant access from `Derived`. Private base members are not accessible.
  Compound accessibility remains unsupported.

## 6. Construction

### 6.1 Sequence

For `new D(arguments)`, where `D : B`:

1. Evaluate the arguments of `new`, left to right (named arguments in source order, as today).
2. Allocate the zeroed object and store the descriptor of `D` in the header.
3. Run the constructor of `D`:
   1. `D`'s field initializers, in declaration order;
   2. the arguments of `base(...)`, left to right;
   3. the constructor of `B`, by the same four steps (and so on up the chain);
   4. the body of `D`'s constructor.
4. Only now does the `new` expression produce its value.

A constructor without `: base(...)` calls an accessible base constructor that needs no arguments (all
parameters defaulted); otherwise it is an error. `base(...)` is only allowed in a constructor of a class
that has a base class.

### 6.2 Comparison with C#

C# runs the same order for the parts that matter here: arguments of `new` first, then allocation, then
the derived field initializers, then the `base(...)` arguments, then the base constructor, then the
derived body. The differences are intentional:

| | C# | B1 |
| --- | --- | --- |
| Virtual call from a base constructor | dispatches to the most derived override, which may see fields that its constructor has not assigned yet | statically rejected (6.3) |
| `this` in a field initializer or in `base(...)` arguments | not allowed | not allowed (as today) |
| Null | fields default to `null` | there is no `null`; a field of a type without a zero default must be definitely assigned |
| Overload priority for derived methods | derived first | no priority |

The original text of this proposal placed the base construction before the derived initializers. That
order is also sound under 6.3, and it is U2; the C# order is proposed because it matches what C#
programmers expect.

### 6.3 Partially initialized objects

The allocator zero-fills storage so that memory is always safe to read. That is an implementation
detail and not a language guarantee: a zeroed class, interface or array field is not a valid
reference. The language therefore must never let program code observe an object until its whole
constructor chain has completed. A runtime flag would tax every dispatch, so B1 uses a compile-time
rule on `this` (rule R, alternatives in U1).

While a constructor runs (its field initializers, the `base(...)` arguments and its body), `this` is
*under construction*, and **for the whole chain**: a `Base` constructor runs on an object whose dynamic
type is `Derived`, so `Derived`'s fields are still unassigned during it.

Under construction, `this` may only be used:

- to read and write fields. A field of a type that has no zero default (class, interface, array,
  string, and so on) must be definitely assigned before it is read and before the constructor
  completes, on every path; primitive value types zero-default. Inherited fields are readable because
  the base constructor has completed;
- as the receiver of a **this-confined** method (below);
- in an identity comparison (`==`, `!=`).

It may not be passed as an argument, assigned, stored in a field, array or collection, returned,
converted to a base class or interface, captured, or used as the receiver of a virtual, abstract,
interface or non-this-confined call. This covers direct calls, calls through helpers, interface
conversions and indirect escape in one local rule.

A method is **this-confined** if it is not virtual or abstract, its body is visible to the compiler, and:

- `this` is only used in its body under the rules above (field access, identity comparison, calls to
  other this-confined methods), so the receiver cannot escape; and
- every method it calls on `this` is this-confined.

Preventing escape is not enough: a helper called on `this` can read a field that the constructor has
not assigned yet. So for each this-confined method `M` the compiler also computes, flow-insensitively
over the whole body and transitively through the this-confined methods it calls, the set `Reads(M)` of
fields of `this` that it may read (property getters included) and requires:

- A call to `M` on `this` from a constructor is allowed only if every field in `Reads(M)` that has no
  zero default (class, interface, array, string and so on) is **definitely assigned at the call site**,
  as computed by the constructor's own flow analysis. Reading a primitive field that has not been
  assigned yields its zero default, as it does today.
- Writes made by a helper are **not** counted as definite assignments in the constructor: only
  assignments in the constructor body itself are. This is deliberately conservative.
- If the compiler cannot compute `Reads(M)` or the this-confinement of `M` (a missing body, a
  recursion that does not reach a fixpoint within the analysis, a construct that the analysis does not
  model, an `async` method, which can suspend with `this` live), `M` is **not** this-confined. Unproven
  means rejected.

The properties are computed for non-virtual methods (including property accessors) by a fixpoint over
the statically known call graph; no annotation is needed. A method that is not this-confined can still
be called on a fully constructed object. Existing code stays valid: the documented property use in a
constructor, `Health -= damage`, reads and writes a primitive field. A constructor that assigns every
reference field before calling a helper that reads them is also valid.

A runtime failure recorded during a constructor stops execution at the next check as it does today; the
host discards the result, so a half-built object is never observed, and the header is valid regardless.

## 7. Conversions

| Conversion | Rule |
| --- | --- |
| `Derived` to `Base` | implicit; same pointer; allowed along the declared chain only |
| `Base` to `Derived` | no implicit conversion; downcast is a checked operation that yields `Option<Derived>` (syntax: U4) |
| unrelated classes | error |
| class to interface | implicit when the **static** class type declares or inherits the interface. `Base` to `IFoo` requires `Base` to implement `IFoo`; if only `Derived` implements it, `Base` to `IFoo` is an error and `Derived` to `IFoo` is allowed |

The interface value stays two pointers: the object and a table. The table is chosen from the dynamic
type, not the static one:

- when the static type is `sealed`, or the compiler knows the exact class, the table is the static one,
  as today;
- otherwise the conversion loads the table from the object's descriptor for that interface. The static
  rule guarantees the descriptor has it. The mechanism for finding it is U3, and the lookup is bounded and validated (4.5).

An override of a virtual method that implements an interface method is what the table points to, so a
call through the interface runs the dynamic override. A derived class may not re-list an interface that
its base already implements (no re-implementation in B1). An abstract class implementing an interface
must declare each interface method, including as `abstract`.

The `==` and `!=` operators keep comparing identity across all of these conversions.

## 8. Generics

- A **closed, fully specialized type** is a non-generic class, or a generic class instantiated only
  with concrete type arguments (`Container<int>`), with no free type parameter anywhere in it. B1
  bases are closed and fully specialized, and may be abstract. The older word "concrete base" is not
  used because it suggests "non-abstract".
- Monomorphization is unchanged. Every closed specialization is its own class symbol with its own
  descriptor, layout and tables, and only closed, fully specialized classes reach HIR, MIR and codegen.
- Whether `class IntContainer : Container<int>` ships in B1 is still the open scope decision U7: it names
  a closed specialization, and it depends on whether the specialization of the base can be requested
  before the derived layout is computed.
- A base that mentions the derived class's own type parameters (`class Box<T> : Container<T>`) is
  open and deferred, and so are substitution of type parameters through the chain, detection of
  infinitely expanding hierarchies, and `where T : BaseClass` constraints. The design must not block
  them.

## 9. Consequences for the compiler and runtime

| Layer | Consequence |
| --- | --- |
| Lexer, parser | `base` becomes a reserved word (it appears in expression position). `virtual`, `override`, `abstract` and `sealed` are contextual: modifiers only before a member or class declaration (U5). Class declarations keep one list after `:`. `TypeRef` stays name-based, so the "interfaces" field becomes a general heritage list |
| Semantic analysis | heritage classification, acyclicity, sealed/abstract checks, override and slot validation, overload candidate collection, `protected` receiver rule, the `this` rule (6.3) and this-confinement fixpoint, definite assignment of fields across the chain, conversion legality |
| Generics | specialization of a base must precede layout of the derived class; identity per specialization |
| HIR | carries the resolved base, slot and override identities, the resolved constructor chain and explicit upcasts; does not redo name or overload resolution |
| MIR | class definitions gain base, flags and slot tables; new operations for upcast, virtual call, interface conversion from a descriptor and the constructor chain; all of them validated independently of the backend (acyclic bases, prefix-compatible layouts, consistent slots, exact override signatures) |
| Optimizer and analyses | an upcast is an alias of the same object; a virtual call is an escape of its receiver until devirtualized; local object elimination must handle receivers of a base static type and the header (an eliminated object never materializes one); lifetime proofs must consider every override as an unknown callee |
| Codegen | field offsets shift by the header; store the descriptor after allocation; emit descriptors (virtual tables and interface tables) the way interface tables are emitted now; indirect calls; reject unsupported shapes before generating code |
| Runtime | no ABI change is required: `aster_rt_object_new` keeps its signature and only receives larger sizes; the runtime does not interpret descriptors |
| Tooling | diagnostics, MIR/HIR dumps and the language server learn the hierarchy |
| Tests and benchmarks | anything that pins an object size or an allocation count needs an audit |

## 10. Unresolved points and alternatives

| ID | Question | Alternatives | Proposal |
| --- | --- | --- | --- |
| U1 | How to prevent observation of a partially initialized object | (A) no use of `this` for calls in constructors, which breaks the documented property use; (B) the this-confinement rule of 6.3, with `Reads(M)` checking and rejection when safety cannot be proven; (C) B plus unrestricted use of `this` in a `sealed` class once its fields are definitely assigned | B; C may be added later |
| U2 | Order of derived initializers and base construction | C# order (6.1); base construction first | C# order |
| U3 | How the conversion finds the interface table | search the descriptor's interface list; a per-hierarchy fixed index assigned to each interface | search first; indexed form only if stage 3 measurements justify it; both are bounded and validated (4.5) |
| U4 | Downcast syntax | `x as Derived` yielding `Option<Derived>`; a `TryFrom` style static call; a pattern form | decide with the downcast PR; the descriptor supports the test either way |
| U5 | `base` as reserved or contextual | contextual with "a declared identifier named `base` wins" | reserved `base` (an identifier named `base` in user code is a migration item; none exists in the repository's own sources) |
| U6 | Header elision for classes outside any hierarchy | keep the header uniform; elide when a class neither derives nor has derived classes nor virtuals | uniform; revisit with the stage 3 measurements |
| U7 | Whether a closed, fully specialized generic base (`Container<int>`) is in B1 scope | in B1; in the first expansion | unresolved scope decision, to be settled before the base-class PR |
| U8 | Covariant return types and `sealed override` | exact signatures and no `sealed override` in B1 | exact |
| U9 | `virtual`/`override`/`abstract` on properties | methods only; properties as accessors | methods only in B1 |
| U10 | Re-listing a base's interface in a derived class | error; C#-style re-implementation | error |

### 10.1 Answers recorded by the architecture closure (2026-11)

The 2026 architecture closure ([decision closure](../proposals/architecture-2026/06-decision-closure.md),
[master plan §6](../proposals/architecture-2026/02-master-plan.md)) answered the open points. The
contracts stay **PROPOSED** until the pull request that implements each part lands.

| Point | Answer | Note |
| --- | --- | --- |
| U1 | Rule B (this-confinement of 6.3) | C may be added later |
| U2 | C# order | 6.1 unchanged |
| U3 | Search the descriptor's interface list | An indexed table only if measured at least 5% faster on `interface_dispatch` through a base type (OBJ-004) |
| U4 | `value is Derived d` is the primary form; `value as Derived` yields `Option<Derived>` | The `T?` spelling depends on LANG-003 and is not assumed |
| U5 | `base` is reserved | No identifier named `base` exists in the repository |
| U6 | Uniform header | Required by the storage-class argument of the master plan |
| U7 | Closed generic bases arrive with the generics-in-hierarchies step (OBJ-006) | Not in the first base-class PR |
| U8 | Exact signatures; no `sealed override` | |
| U9 | `virtual`/`override`/`abstract` on methods **and property accessors** (Q-OBJ-2) | Differs from this record's own recommendation. A virtual property on `this` inside a constructor is rejected like N11; P8 stays valid for non-virtual properties only |
| U10 | Error | |
| Header | `H = P` (H8) | Q-OBJ-1 after MEM-EXP-1 FAIL; T-MEM-2 merges this header |

Amendments from the closure that do not change a contract here: the descriptor may gain fields
(nothing is removed); descriptors are immortal module data in production and AOT, and live in
session-stable storage in the development profile (HOT-002); class references stay
non-transferable; a lambda that captures `this` inside a constructor is a new negative case; `sealed`
does not make a hierarchy closed (exhaustive `switch` applies to enums only).

## 11. Stages, authorization and the sequence of pull requests

Design acceptance, experimental implementation, performance validation and authorization are separate
stages. A stage never needs a result that only a later stage produces.

| Stage | What it is | May merge to `main`? | Gate to the next stage |
| --- | --- | --- | --- |
| 1. Design acceptance | the maintainer accepts this record: U1 to U10 decided (U6 and the choice in U3 may be left to stage 3), contracts of sections 4 to 8 fixed, public documents prepared | documentation only | formal acceptance in writing |
| 2. Experimental implementation | the minimum needed to measure: every class gets the header and a descriptor with its interface tables, no hierarchy and no user-visible syntax; semantics of existing programs unchanged, except sizes | only as a draft pull request or an unmerged branch; it merges only if stage 3 passes | a measurement report can be produced |
| 3. Performance validation | measure allocation size, allocation volume, and time on the existing benchmarks and test suites, against thresholds that the maintainer records before the run | the stage 2 change, if the result is accepted | the maintainer accepts the measured cost or chooses U6 header elision or another mitigation |
| 4. Authorization of the feature stages | the maintainer authorizes the pull requests below one by one | each one when authorized | per-pull-request review |

Pull requests after stage 3, each with its own tests and documentation, and none before its
authorization. No public page describes inheritance before the pull request that implements it.

1. **Single inheritance.** Base class (abstract or not), layout of 4.1, field inheritance, upcast,
   `base(...)`, `protected`, construction order and the `this` rule.
2. **Virtual dispatch.** `virtual`, `override`, `abstract`, `sealed`, `base.Method`, escape analysis and
   object elimination updates, descriptor validation.
3. **Interfaces through hierarchies.** Conversion from a base type with the dynamic table and the
   validated lookup.
4. **Downcast** to `Option<Derived>`.
5. **Generics in hierarchies**, in the order of section 8 (including U7 if it was left out).

Constructor overloads and `this(...)` (B2) are independent and not part of this sequence.

**Delivery note, pull request 1 (T-OBJ-2).** Implemented: class bases (abstract or not; closed and
fully specialized only, so `class D : Box<int>` works and an open base is rejected, which settles U7
for this step), the 4.1 layout with inherited offsets preserved and independently recomputed by the
backend layout validation (V1, V5), `Upcast` in HIR and MIR (validated: only along the declared
chain), `base(...)` and the C# construction order of 6.1, `protected`, the rule B this-confinement of
6.3 with `Reads(M)` (N10, N12, N13, N20 to N22), `abstract`/`sealed` classes, reserved `base` (U5), and
the descriptor base pointer, depth and ancestors array of 4.2 (V3, V4). Cases covered: P2, P3, P6, P8,
P9, P10 to P15 and N1, N4 to N6, N8 to N10, N12 to N18, N20 to N22; N2, N3 and N7 are rejected with a
"not supported yet" diagnostic because they need `virtual`/`override`/`abstract` members (pull request
2). An interface conversion from a derived class reuses the static table of the converted class,
which is exact while no member is virtual; the dynamic lookup is pull request 3. Local object
elimination skips every class that has a base. Not implemented: virtual dispatch, downcast, open bases.

## 12. Statements to supersede on acceptance

`docs/reference/classes.md`, `docs/reference/interfaces.md` ("The name after `:` is an interface"),
`docs/reference/generics.md`, `docs/reference/generic-types.md`, `docs/compiler/grammar.md`,
`docs/compiler/architecture.md` (deliberate boundaries and execution notes),
`docs/compiler/execution-context.md`, `docs/specification/10-open-questions.md`,
`docs/specification/11-generics.md`, `docs/specification/15-visibility.md` (the `protected` open
question) and `docs/language-tour.md`. They stay unchanged until the implementing pull request.

## 13. Cases

Positive:

| ID | Case | Expected |
| --- | --- | --- |
| P1 | `Entity e = new Enemy(10); e.Update(1.0f);` with an `Enemy` override | runs `Enemy.Update` |
| P2 | `Enemy(int health) : base(health)` | the base field is initialized before the body |
| P3 | `Entity a = enemy; a == enemy` | `true`; same identity, no copy |
| P4 | an abstract class with a concrete derived class overriding every abstract method | compiles and dispatches |
| P5 | `IScore s = baseRef;` where `Base : IScore`, dynamic type `Derived` overriding `Value()` | the call runs `Derived.Value` |
| P6 | `protected` field read in `Derived` through `this` or a `Derived` receiver | allowed |
| P7 | `base.Update(dt)` in an override | runs the base implementation without redispatch |
| P8 | constructor with `Health -= damage` (property on `this`) | allowed: the accessors are this-confined |
| P9 | initializer, `base(...)` argument and body each logging | the order of 6.1 |
| P10 | `sealed class Leaf : Entity` | compiles; `Leaf` to an interface uses the static table |
| P11 | `Base { long x; byte b }` and `Derived : Base { byte d }` | `x`, `b` keep offsets 8 and 16 in `Derived`; `d` is at 24 and `size(Derived)` is 32 |
| P12 | a class with no fields, and a derived class with no own fields | sizes 8 and `size(Base)`; instantiable |
| P13 | `abstract class Shape { protected int id; public Shape(int id) { this.id = id; } }`, `class Circle : Shape` with `: base(id)` | an abstract class is inherited; its constructor and field are reached |
| P14 | a constructor assigns a reference field, then calls a this-confined helper that reads it | allowed: `Reads(M)` is assigned at the call site |
| P15 | a helper that reads only a primitive field, called before the field is assigned | allowed: the zero default is read |

Negative (each a controlled compile-time diagnostic):

| ID | Case |
| --- | --- |
| N1 | `class A : B`, `class B : A` (and any longer cycle) |
| N2 | `override` of a method that is not virtual or abstract, or that does not exist |
| N3 | `override` with different parameters, return type or weaker visibility |
| N4 | a same-signature method without `override` (silent hiding) |
| N5 | deriving from a `sealed` class, a struct, an enum, an interface or a static class as a base |
| N6 | `new` on an abstract class |
| N7 | a concrete class that does not override an inherited abstract method |
| N8 | a conversion between unrelated classes; an implicit `Base` to `Derived` |
| N9 | `Base` to `IFoo` when only `Derived` implements `IFoo` |
| N10 | `this` passed, stored, returned or converted (to a base or interface) inside a constructor |
| N11 | `this.Virtual()` or an implicit virtual call inside a constructor |
| N12 | a constructor calling a helper that is not this-confined (it calls a virtual method, or passes `this` on) |
| N13 | reading a field before it is definitely assigned, or finishing a constructor with an unassigned reference field |
| N14 | a private base member, or a `protected` member through a `Base`-typed receiver, accessed from `Derived` |
| N15 | `base(...)` in a class without a base class or outside a constructor; a base constructor that needs arguments with no `base(...)` |
| N16 | two class bases; a class after an interface in the list; re-listing an interface the base already implements |
| N17 | `abstract` and `sealed` together; an abstract method in a non-abstract class; `virtual` on a static, private or constructor member |
| N18 | an open base: a base type that mentions the derived class's own type parameters (until the expansion) |
| N19 | a class reference sent to a worker (still rejected) |
| N20 | a constructor calls a helper (this-confined) that reads a reference field not yet definitely assigned |
| N21 | a helper whose only assignment to a reference field is inside another helper, followed by a read of it in the constructor (helper writes do not count) |
| N22 | a constructor calls a helper whose this-confinement or reads cannot be computed (an `async` method; a body that is not available) |

Validator and descriptor cases (internal invariants, checked when the module is built and tested with
hand-written MIR or descriptors; none of them is reachable from valid source):

| ID | Case | Expected |
| --- | --- | --- |
| V1 | a derived layout whose inherited field offset differs from the base, or whose first own field is inside `size(B)` | rejected before code generation |
| V2 | a store to offset `0` other than the allocation sequence | rejected |
| V3 | a descriptor whose `depth` is not the base's plus one, or whose `ancestors` length or prefix is wrong | rejected |
| V4 | a subtype test of a deep target against an object of a shallow class | `false`, with no read past the `ancestors` array |
| V5 | a cycle in the base chain | rejected |
| V6 | a non-abstract class with no table for an interface its static type implements, or with a table of the wrong method count | rejected |
| V7 | an interface lookup that finds no table at run time (hand-built) | a controlled runtime error; no call through a missing or null table |
| V8 | a virtual call whose slot index is not below the static type's vtable length | rejected |

## 14. Criteria for formal acceptance

Formal acceptance (stage 1 of section 11) requires all of the following. None of them needs a
measurement from an implementation.

1. U1 to U10 are decided and recorded, except U6 and the choice in U3, which stage 3 may settle.
2. The construction sequence, the `this` rule, this-confinement and the `Reads(M)` check are specified
   without open edge cases, and the positive and negative cases above are converted into planned
   regression tests.
3. Header, offsets (4.1), descriptor contents, validation (4.5) and lifetime (4.3) are fixed enough to
   implement and to validate independently; it is confirmed that no task or worker can run module
   code after the module is released. The measurement protocol for stage 3 (what is measured, on
   which benchmarks, and who records the thresholds) is written down.
4. The interaction with interfaces, generic specialization and `where` constraints is specified for B1,
   including the definition of a closed, fully specialized base type and the decision on U7.
5. The consequences for escape analysis, local object elimination and the lifetime proofs are written
   down per analysis.
6. HIR and MIR shapes and their independent backend validation are specified.
7. Every invalid hierarchy has a controlled diagnostic.
8. Compatibility with existing programs is confirmed (the only source-visible change is the reserved
   word `base`; classes grow by the header).
9. The documents in section 12 have a prepared update.
10. The maintainer accepts this record.

Stage 2 may then begin. Authorization of the feature pull requests (stage 4) additionally requires the
accepted result of stage 3.
