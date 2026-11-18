# Design record: Object model B2 (constructor overloads and `this(...)`)

**Status: IMPLEMENTED** by T-OBJ-5 (register row OBJ-005, closure Q-OBJ-3). This record was written
before the implementation and is updated with the delivery note in section 8. It extends
[object model B1](object-model-b1.md), whose construction order (section 6.1) and this-confinement
rule B (section 6.3) it keeps.

| | |
| --- | --- |
| Related | [object model B1](object-model-b1.md), [classes](../reference/classes.md), [decision closure](../proposals/architecture-2026/06-decision-closure.md) (Q-OBJ-3) |
| Replaces | the B1 statements "one constructor per class" and "`this(...)` is not supported yet" |

## 1. Scope

A class may declare several constructors, overloaded by parameter types, and a constructor may
delegate to another constructor of the same class with `: this(arguments)`. Initializers run exactly
once per object, in the constructor that calls `base(...)`, explicitly or implicitly.

Not in scope: structs and enums (they have no constructors), static classes, `new` target typing,
object-initializer syntax, constructor type parameters, and any change to the descriptor, layout,
runtime ABI or the arena model. The default stays C0.

## 2. Contract

1. **Declaration.** Constructors are distinguished by their parameter types, as methods are. Two
   constructors with the same parameter types (names and defaults do not count) are an error
   ("duplicate constructor"). Each constructor has its own visibility.
2. **Implicit constructor.** The compiler synthesizes the parameterless public constructor only for a
   class that declares no constructor (and whose fields all initialize or zero-default, as in B1). A
   class with any explicit constructor has no implicit one, as in C#.
3. **`new C(args)`.** Overload resolution is the existing rule of semantic analysis (named arguments
   and defaults included): the candidates are the constructors accessible from the call site; an exact
   match beats a safe implicit conversion; equally good candidates are an error ("call to `C` is
   ambiguous"); no applicable candidate is an error ("no overload of `C` accepts these arguments", or
   the specific binding error when there is exactly one candidate). A parameterless `new C()` needs
   a constructor that accepts no arguments. A call whose only constructors are inaccessible is
   reported as private or protected.
4. **`base(args)`.** Resolved the same way among the accessible (non-private) constructors of the
   base class. Without `: base(...)` and without `: this(...)`, the constructor needs a base
   constructor whose parameters are all defaulted (an ambiguity among several is an error).
5. **`: this(args)`.** Resolved among all constructors of the same class, private ones included. It
   is allowed only on a constructor. `this(...)` and `base(...)` on one constructor are an error (a
   constructor has one initializer, and a delegating constructor reaches `base(...)` through its
   target).
6. **Cycles.** If following `this(...)` from a constructor returns to a constructor already visited
   (directly, `C() : this()`, or through others), the program is rejected once per cycle, at the first
   constructor of the cycle in source order. Detection uses the resolved targets, so it follows
   overload resolution, not names.
7. **Definite assignment.** A constructor that delegates starts with every field of the object (own
   and inherited) assigned, because its target has completed. Get-only and `init` auto-properties
   (T-LANG-5) are assignable in every constructor of the declaring class, delegating ones included.

## 3. Construction order with delegation

For `new D(arguments)`: evaluate the arguments of `new`, allocate, store the descriptor (B1 6.1), and
run the selected constructor `K`:

- If `K` does **not** delegate: `D`'s field initializers in declaration order, the arguments of
  `base(...)` left to right (absent for a class with no base), the base constructor chosen by B1/B2
  rules, then `K`'s body.
- If `K` delegates with `this(args)`: the arguments of `this(...)` left to right, then the target
  constructor `T` (which runs this same sequence), then `K`'s body.

So initializers and the base constructor run exactly once however long the chain is, and bodies run
from the innermost (the end of the chain) outward. A chain `K1 -> K2 -> K3` with `K3 : base(...)`
runs: `K1`'s `this` arguments, `K2`'s `this` arguments, `D`'s initializers, `K3`'s `base`
arguments, the base constructor, `K3`'s body, `K2`'s body, `K1`'s body. A delegating constructor
emits no initializer assignments (they are visible in the MIR as a call followed by the body).

## 4. Interaction with this-confinement (B1 6.3, rule B)

- The arguments of `this(...)` are analyzed with no object, exactly like `base(...)` arguments:
  `this`, fields, properties and instance methods of the class cannot be used. Diagnostics name
  `this(...)`.
- Rule B holds in every constructor of the chain. `this` is under construction in the target, in
  the base chain and in the delegating body alike; a delegating body may use `this` only as B1 6.3
  allows. Because all fields count as assigned after a delegation, the `Reads(M)` check never
  reports an unassigned field in a delegating body.
- Helpers called from a constructor are this-confined or rejected independently of which constructor
  calls them.

## 5. Interaction with other analyses

- Local object elimination recognizes only constructors whose whole body is simple field stores; a
  delegating constructor contains a call, so an object built through it is never scalarized. The
  other overloads are analyzed one by one. Escape analysis and lifetime analysis see ordinary calls
  with the receiver as first argument.
- No change to MIR (a delegation is a `Call` of a constructor function with the same receiver) or to
  codegen. HIR lowering emits the call as the first statement of a delegating constructor.

## 6. Negative cases

| ID | Case |
| --- | --- |
| C1 | `C() : this()` and any longer cycle through `this(...)`, resolved by overload |
| C2 | `: this(...)` and `: base(...)` together (either order) |
| C3 | an ambiguous overload in `new`, `this(...)` or `base(...)` |
| C4 | no applicable constructor |
| C5 | `this`, a field or an instance member in the arguments of `this(...)` |
| C6 | two constructors with the same parameter types |
| C7 | `this(...)` outside a constructor |
| C8 | a class with an explicit constructor instantiated with no matching one (no implicit default) |
| C9 | a private constructor outside its class (also through an overload set with no accessible match) |

## 7. Decisions and tie-break

- Ties are errors and a constructor without default parameters has no priority over one with them: `A(int)` and
  `A(int, int b = 0)` called with one argument are ambiguous, as the existing method rule
  decides. This differs from C#.
- Accessibility filters the candidates before ranking; C# reports an inaccessible best match
  instead. The effect is a more specific "too many arguments" or "no overload" message when a
  private overload would have matched.

## 8. Delivery note (T-OBJ-5)

Implemented: `BaseCall.delegates_to_this` in the syntax tree (`this(...)` is parsed; a second
initializer is a parse error), `TypeInfo.constructors` and the duplicate-signature check
(declarations), overload resolution in `new`, `base(...)` and `this(...)`
(`calls.rs`, `inheritance.rs::analyze_base_call`), the delegation cycle check
(`inheritance.rs::check_delegation_cycles`), definite assignment after delegation, and the HIR
prologue that omits initializers in a delegating constructor. The resolved target of both `base(...)`
and `this(...)` is stored in `Model::base_constructors` keyed by the constructor's model context.
Cases C1 to C9 are covered by `crates/aster-compiler/tests/constructor_overloads.rs`; execution
order, chains of three, base overloads, initializer-once and auto-property assignment by
`crates/aster-codegen-cranelift/tests/constructor_overloads.rs`.
