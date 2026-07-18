# 03 — Variables and constants

## Objective

Define C#-style local variables, inferred variables, compile-time constants, assignment,
and the remaining design space for write-once fields.

## Accepted syntax

```aster
int score = 0;
var name = "Natte";
const int MaxScore = 100;

score += 10;
name = "Aster";
```

## Accepted rules

- `Type name = expression;` declares a mutable variable with an explicit type.
- `var name = expression;` declares a mutable variable whose type is inferred from its
  initializer. `var` is not a type.
- `const Type name = expression;` declares a compile-time constant.
- Variables are mutable by default; no `var Type name` form exists.
- A `var` declaration requires an initializer that determines one unambiguous type.
- Assignment requires a mutable variable and a compatible value.
- Reading a local before definite initialization is an error.
- Redeclaring a name in the same lexical scope is an error.

### Constant expressions (implemented)

`const` initializers must be compile-time constant expressions: literals, references to
previously declared constants, the arithmetic/comparison/logical operators, `?:`, string
`==`/`!=`/`+` (concatenation folds at compile time), and explicit casts. Calls, variables,
and `++`/`--` are rejected. Constant expressions are evaluated with the same semantics the
JIT uses at runtime, and the compiler reports overflow (`constant expression overflows
`int``) and division or remainder by zero as errors. Evaluated constants are folded into
literals during lowering, which also makes namespace-level `const` values usable from executed
code.

`readonly` is reserved as a design candidate for fields assigned once. It is not accepted
for locals or fields yet.

## Valid design examples

```aster
int lives = 3;
lives -= 1;

var playerName = "Natte";
playerName = "Aster";

const float Pi = 3.14159;
```

## Invalid design examples

```aster
var int attempts = 3;       // removed syntax: var is followed by a name
var missing;                // inference requires an initializer
const int answer = read();  // runtime call is not a compile-time expression
int score = "zero";        // incompatible initializer
```

## OPEN QUESTIONS

### PROPOSED — Write-once fields

Should fields that can be assigned only during initialization use `readonly`?

1. **`readonly Type name;`** — familiar to C# users and states the restriction directly;
   it adds a dedicated modifier and requires precise constructor/initializer rules.
2. **`const` fields** — fewer keywords, but conflates compile-time constants with per-instance
   write-once values.
3. **Immutable object/struct declarations** — stronger aggregate guarantees, but cannot
   express a single write-once field in an otherwise mutable type.

**Recommendation:** PROPOSED — use `readonly Type name;` for instance fields, assignable
only during object initialization or construction. Constructor rules must be decided first.

### PROPOSED — Nested-scope shadowing

1. **Allow shadowing freely** — concise transformations; accidental name reuse is easier.
2. **Forbid all shadowing** — simplest diagnostics; makes small nested scopes verbose.
3. **Allow with an explicit marker or warning** — makes intent visible; adds syntax or policy.

**Recommendation:** PROPOSED — forbid same-scope redeclaration and warn on nested shadowing
initially, postponing an explicit marker until real examples justify it.

### PROPOSED — Mutable namespace-level variables

1. **Allow them** — simple shared state; complicates initialization, testing, and concurrency.
2. **Allow only `const` namespace values** — predictable and safe; forces state behind explicit APIs.
3. **Allow mutable fields only inside managed resources** — supports stateful programs; couples
   core language rules to a library abstraction.

**Recommendation:** PROPOSED — allow only compile-time `const` values at namespace scope in the
initial language. Revisit explicit runtime global state independently of ECS resources.

### PROPOSED — Compile-time expression boundary

1. **Literals and operators only** — deterministic and easy to implement; limited reuse.
2. **Also permit calls to marked constant functions** — expressive; requires a constant evaluator.
3. **Run arbitrary user code at compile time** — most powerful; raises security and reproducibility costs.

**Recommendation:** PROPOSED — begin with literals, references to other constants, aggregate
construction, and deterministic operators; add explicitly marked constant functions later.
