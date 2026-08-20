# Compatibility before 1.0

ASTER is still a `0.x` language. The compiler can evolve before 1.0, but changes should not make
users guess which parts of the toolchain are safe to rely on. This page defines the compatibility
contract for documented, implemented behavior.

## Stability bands

| Surface | Patch release | Minor release before 1.0 |
| --- | --- | --- |
| Source language | Preserve documented valid programs. Correctness or safety fixes may reject behavior that was invalid, unsafe, or accepted by mistake. | May deliberately change documented syntax or semantics when the language needs it. The migration must be documented and covered by diagnostics where practical. |
| `Aster.toml` | Preserve the current documented manifest contract. | May deliberately change the manifest when the migration is documented and diagnosed where practical. |
| Standard library | Preserve documented names and behavior except for correctness or safety fixes. Additive APIs are allowed. | May rename, replace, or remove APIs when the replacement is documented and the migration is clear. |
| CLI | Preserve documented commands, flags, exit-code meanings, and stdout/stderr roles. | May reshape a command contract when the migration is documented. Human diagnostic wording is not a byte-for-byte compatibility surface unless a specific diagnostic contract says otherwise. |
| Release and install artifacts | Preserve supported platform targets and the validated install, repair, update, rollback, and uninstall contract. | Packaging may evolve when the supported replacement and migration are documented and validated on every affected platform. |
| Native runtime ABI | No public stability promise yet. | No public stability promise yet. A stable native ABI requires an explicit future contract. |

A patch should therefore be conservative. An urgent correctness or safety repair can narrow behavior,
but it must have regression coverage and documentation when users could observe the difference.

## What is not a compatibility promise

Parser acceptance by itself is not a language guarantee. Research notes, unsupported syntax,
compiler-internal Rust APIs, cache layouts, allocation implementation details, and the textual shape
of HIR/MIR dumps may change without a source-compatibility promise. HIR and MIR remain typed
compiler contracts internally, but their debug rendering is not a serialized public format.
Compiler-proven unobservable intermediate allocations are not a source-level compatibility
guarantee. The optimizer may eliminate or replace them, including scalar-replaced local objects and
compiler-replaced string-construction intermediates. Runtime memory statistics and resource-budget
consumption describe allocations that remain in executable MIR after optimization, not a
hypothetical unoptimized execution. An eliminated or replaced unobservable allocation loses its
former resource-failure point; observable operations and allocations that still execute retain
their established ordering and controlled-failure behavior.

The general MIR optimizer follows the same boundary for scalar work. It may fold and remove dead
primitive assignments only when evaluation is pure and non-failing. Calls, allocations, bounds-checked
access, integer division/remainder, host operations, Task/Parallel operations, and collection mutation
remain executable even when their result local is unused. MIR integer folding uses runtime wrapping
semantics; it does not turn a valid runtime expression into constant-declaration overflow checking.

Compiler-proven last-use reclamation is likewise not a source-semantic change: references keep
their documented identity, copy, equality, and mutation behavior while live. An owned-region
rewind occurs only after the complete proven alias closure is dead, does not move live storage, and
does not run finalizers. Shared, contained, cyclic, interface, cross-worker, and otherwise uncertain
shapes retain the conservative execution-context lifetime.

Task argument transfer and composition preserve the same source-level boundary: arguments are
caller-evaluated value copies, worker results remain scalar, and no reference identity crosses an
execution context. `Task.WaitAll` orders successful results and selected failures by input index,
not scheduler completion. Cooperative cancellation changes only the terminal outcome of a task
whose request was accepted; it does not forcibly stop a worker or introduce hidden polling.

The absence of a diagnostic for an unsupported program is also not a promise that the behavior is
supported. Unsupported execution must continue to fail before unsafe code generation.

## Project manifests

`Aster.toml` has one current format. Every manifest-backed package declares its identity with
`[package] name`; `[application]` is optional, and local path dependencies use `[dependencies]`:

```toml
[package]
name = "app"

[application]
entry = "app.Program.Main"

[dependencies]
math = { path = "../math" }
```

ASTER is pre-1.0, so this syntax may still evolve before 1.0. Incompatible manifest changes must be
intentional, documented, and produce controlled migration diagnostics where practical. There is no
in-file schema, edition, or format-version mechanism. An obsolete `schema` field is rejected; it
never selects alternate parser or identity behavior. See [packages and dependencies](packages.md).

Git source dependencies extend this model through `Aster.lock`: the declared package name still
owns nominal identity, while the lockfile records only the exact immutable Git resolution.

## Incompatible changes during 0.x

A planned incompatible change before 1.0 must:

1. use an accurate Conventional Commit breaking marker (`!` or `BREAKING CHANGE:`);
2. ship in a minor release, not a patch;
3. update the canonical reference or guide in the same change;
4. explain the old and new form and provide a migration diagnostic when the compiler can identify
   the old form reliably;
5. preserve controlled failure through semantic analysis, MIR validation, or another owning layer
   rather than letting unsupported behavior reach unsafe execution.

The release analyzer intentionally maps breaking commits to a **minor** release while ASTER remains
`0.x`. Before the project deliberately enters 1.0, that temporary rule must be removed so later
breaking commits regain normal major-version semantics. The 1.0 transition is therefore an explicit
project decision, not an accidental consequence of one pre-1.0 language experiment.

ASTER does not use language editions today. If editions ever become useful, they require a separate
design and are not implied by this compatibility policy.
