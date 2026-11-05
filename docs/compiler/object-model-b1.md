# Architectural proposal: Object model B1 (single class inheritance)

**Status.** Direction B (nominal single class inheritance with polymorphism, native execution, no GC,
no .NET dependency) is **ACCEPTED as the objective**. Every B1 contract in this document is
**PROPOSED**. Nothing here is implemented, and no public documentation may describe inheritance as
available until the pull request that implements a part of it lands.

| | |
| --- | --- |
| Baseline | `c3ef946`, release 0.71.0 |
| Related | [classes](../reference/classes.md), [interfaces](../reference/interfaces.md), [generic types](../reference/generic-types.md), [monomorphization](monomorphization.md), [runtime ABI](runtime-abi.md), [execution context](execution-context.md), [visibility](../specification/15-visibility.md) |
| Supersedes (on acceptance) | the "class inheritance is not adopted" statements listed in section 12 |

## 1. Scope

B1 adds single implementation inheritance, `virtual`/`override`/`abstract`/`sealed`, `base(...)`
construction, `protected`, and class-to-base and class-to-interface conversions that respect the
dynamic type. It does not add `object` as a universal root, boxing, reflection, attributes, static
fields, delegates, events, GC, finalizers, multiple implementation inheritance, or .NET binary
compatibility. Each of those needs its own record.

## 2. Approved decisions

These were approved by the maintainer and are inputs to this proposal.

1. **Uniform header.** Every class instance has a header, with a pointer to its dynamic descriptor at
   offset zero.
2. **Immutable descriptors.** One descriptor per concrete class or generic specialization. It carries
   the hierarchy, the virtual dispatch table and the interface tables.
3. **Constructors split.** B1 keeps one constructor per class and adds `base(...)`. B2 (separate
   record) adds constructor overloads and `this(...)`.
4. **Concrete bases only.** A B1 base class is a concrete type. Inheriting from a base parameterized by
   open type parameters is a later expansion that this design must not block.
5. **Identity-preserving upcast.** Upcasting never copies. Dispatch and conversions follow the dynamic
   type.

## 3. Facts about the current implementation that shape the design

Verified in the repository at the baseline:

- Objects are headerless. `aster_rt_object_new(context, size)` returns zeroed storage owned by the
  `ExecutionContext`; no type identity exists at run time.
- `mir::Type::Class(SymbolId)` is an exact type. MIR has no subtype relation.
- Class layout is computed as a struct: fields in declaration order with natural alignment
  (`layouts.rs`). A class with no fields occupies one byte.
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

```text
offset 0                 descriptor pointer (pointer-sized, naturally aligned)
offset H                 base-class fields (H = pointer size, rounded up to the first field's alignment)
offset after base        derived fields, each at its natural alignment
```

- The base layout is a strict prefix of the derived layout. A field keeps the offset it has in the
  base, so code compiled against `Base` works on any `Derived`. Derived fields start at the end of the
  base's fields, not in the base's tail padding.
- An upcast is the same pointer. No conversion code runs.
- The descriptor pointer is written at allocation time, immediately after `aster_rt_object_new`, with
  the descriptor of the **concrete** class. It is therefore valid from the first instruction after
  allocation, including in an invocation that later records a controlled runtime error.
- The layout is an internal ASTER contract, not a C or C# ABI.

### 4.2 Descriptor contents (PROPOSED shape, not a binary format)

| Field | Purpose |
| --- | --- |
| class identity | unique per concrete class or specialization; `Container<int>` and `Container<string>` differ |
| base descriptor, depth, ancestor array | constant-time subtype test: `ancestors[target.depth] == target` |
| flags | abstract, sealed |
| instance size | for diagnostics and validation |
| virtual table | one slot per introduced virtual method; overrides reuse the slot; an abstract slot holds a stub that records a controlled runtime error |
| interface tables | for each implemented interface (declared or inherited), the table of concrete methods |

### 4.3 Lifetime

Descriptors are immutable data owned by the JIT module, defined the same way interface tables are
today. They live as long as the module, which outlives every invocation that uses it; objects and
interface values hold raw pointers that never outlive their invocation. Class references do not cross
workers, so descriptor identity is only compared inside one module. A watch rebuild creates a new
module and never mixes descriptors. Nothing frees a descriptor.

### 4.4 Cost

Every object grows by one pointer, and by alignment. A class with one `int` field goes from 4 bytes to
16. This affects allocation sizes in arena accounting, temporary-subregion size estimates and any test
or benchmark that pins object sizes. The first implementation PR must measure allocation volume and
time on the existing benchmarks before the cost is accepted.

## 5. Inheritance and polymorphism

- A class has at most one base class and any number of interfaces. A single list after `:` names them;
  semantic analysis classifies each name as class or interface. A class base, if present, must be
  first. Structs, enums, interfaces and static classes cannot be bases. Circular inheritance is an error.
- Inherited members that are accessible are members of the derived class. A derived class does not
  duplicate base storage.
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

A method is **this-confined** if it is not virtual or abstract and, in its body, `this` is only used
under the same rules as above and every method it calls on `this` is this-confined. The property is
computed by the compiler for non-virtual methods (including property accessors) by a fixpoint over the
statically known call graph; it needs no annotation. A method that is not this-confined can still be
called on a fully constructed object. Existing code stays valid: the documented property use in a
constructor, `Health -= damage`, touches only fields.

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
  rule guarantees the descriptor has it. The mechanism for finding it is U3.

An override of a virtual method that implements an interface method is what the table points to, so a
call through the interface runs the dynamic override. A derived class may not re-list an interface that
its base already implements (no re-implementation in B1). An abstract class implementing an interface
must declare each interface method, including as `abstract`.

The `==` and `!=` operators keep comparing identity across all of these conversions.

## 8. Generics

- Monomorphization is unchanged. Every closed specialization is its own class symbol with its own
  descriptor, layout and tables, and only concrete classes reach HIR, MIR and codegen.
- B1 bases are concrete. `class IntContainer : Container<int>` names a closed specialization: U7 asks
  whether it ships in B1 or in the first expansion, depending on whether the specialization of the base
  can be requested before the derived layout.
- Deferred, and not to be blocked by the design: a base parameterized by open type parameters
  (`class Box<T> : Container<T>`), substitution of type parameters through the chain, detection of
  infinitely expanding hierarchies, and `where T : BaseClass` constraints.

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
| U1 | How to prevent observation of a partially initialized object | (A) no use of `this` for calls in constructors, which breaks the documented property use; (B) the this-confinement rule of 6.3; (C) B plus unrestricted use of `this` in a `sealed` class once its fields are definitely assigned | B; C may be added later |
| U2 | Order of derived initializers and base construction | C# order (6.1); base construction first | C# order |
| U3 | How the conversion finds the interface table | search the descriptor's interface list; a per-hierarchy fixed index assigned to each interface | search first; indexed form only if measurements justify it |
| U4 | Downcast syntax | `x as Derived` yielding `Option<Derived>`; a `TryFrom` style static call; a pattern form | decide with the downcast PR; the descriptor supports the test either way |
| U5 | `base` as reserved or contextual | contextual with "a declared identifier named `base` wins" | reserved `base` (an identifier named `base` in user code is a migration item; none exists in the repository's own sources) |
| U6 | Header elision for classes outside any hierarchy | keep the header uniform; elide when a class neither derives nor has derived classes nor virtuals | uniform; revisit with the measurements of 4.4 |
| U7 | A closed generic base (`Container<int>`) in B1 | in B1; in the first expansion | decide in the base-class PR |
| U8 | Covariant return types and `sealed override` | exact signatures and no `sealed override` in B1 | exact |
| U9 | `virtual`/`override`/`abstract` on properties | methods only; properties as accessors | methods only in B1 |
| U10 | Re-listing a base's interface in a derived class | error; C#-style re-implementation | error |

## 11. Sequence of future vertical pull requests

Each lands behind its own acceptance, with tests and documentation for exactly what it adds. No public
page describes inheritance before the pull request that implements it.

1. **Acceptance.** Record the final contracts, resolve U1 to U10, and update the documents in section
   12.
2. **Header and descriptors.** Every class gets the header and a descriptor with its interface tables;
   no hierarchy yet. Measure allocation size, allocation volume and time.
3. **Single inheritance.** Base class, layout prefix, field inheritance, upcast, `base(...)`, `protected`,
   construction order and the `this` rule.
4. **Virtual dispatch.** `virtual`, `override`, `abstract`, `sealed`, `base.Method`, escape analysis and
   object elimination updates.
5. **Interfaces through hierarchies.** Conversion from a base type with the dynamic table.
6. **Downcast** to `Option<Derived>`.
7. **Generics in hierarchies**, in the order of section 8.

Constructor overloads and `this(...)` (B2) are independent and not part of this sequence.

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
| N18 | an open generic base (until the expansion) |
| N19 | a class reference sent to a worker (still rejected) |

## 14. Criteria for formal acceptance

All of the following must hold before any implementation pull request:

1. U1 to U10 are decided and recorded.
2. The construction sequence, the `this` rule and the this-confinement computation are specified without
   open edge cases, with the positive and negative cases above converted into planned regression tests.
3. Header, offsets, descriptor contents and lifetime are fixed enough to implement and validate, and the
   cost in 4.4 has been measured on the existing benchmarks and accepted.
4. The interaction with interfaces, generic specialization and `where` constraints is specified for B1.
5. The consequences for escape analysis, local object elimination and the lifetime proofs are written
   down per analysis.
6. HIR and MIR shapes and their independent backend validation are specified.
7. Every invalid hierarchy has a controlled diagnostic.
8. Compatibility with existing programs is confirmed (the only source-visible change is the reserved
   word `base`; classes grow by the header).
9. The documents in section 12 have a prepared update.
10. The maintainer accepts this record.
