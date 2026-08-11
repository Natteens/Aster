# Compatibility before 1.0

ASTER is still a `0.x` language. The compiler can evolve before 1.0, but changes should not make
users guess which parts of the toolchain are safe to rely on. This page defines the compatibility
contract for documented, implemented behavior.

## Stability bands

| Surface | Patch release | Minor release before 1.0 |
| --- | --- | --- |
| Source language | Preserve documented valid programs. Correctness or safety fixes may reject behavior that was invalid, unsafe, or accepted by mistake. | May deliberately change documented syntax or semantics when the language needs it. The migration must be documented and covered by diagnostics where practical. |
| `Aster.toml` | Preserve the current manifest schema and its documented interpretation. | May introduce a new explicit schema or project contract. Unsupported newer schemas must fail with a controlled diagnostic. |
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

The absence of a diagnostic for an unsupported program is also not a promise that the behavior is
supported. Unsupported execution must continue to fail before unsafe code generation.

## Project manifests

`Aster.toml` has an explicit schema authority. Each schema number keeps exactly one documented
meaning, and an unsupported schema is rejected rather than guessed.

Schema `1` accepts only `schema` and a required `[application]` table containing only `entry`:

```toml
schema = 1

[application]
entry = "app.Program.Main"
```

Manifests that omit `schema` are interpreted as schema `1`. Schema `1` is unchanged and keeps
working; it simply has no package identity and therefore cannot declare dependencies.

Schema `2` adds package identity and local path dependencies, and makes `[application]` optional so
a package can be a library:

```toml
schema = 2

[package]
name = "app"

[application]
entry = "app.Program.Main"

[dependencies]
math = { path = "../math" }
```

Schema `2` was introduced additively rather than by reinterpreting schema `1`, because schema `1`
deliberately rejects unknown fields. `aster new` writes the newest schema. See
[packages and dependencies](packages.md).

Future schema changes must keep the same rule: one documented schema meaning per number,
deterministic discovery, and a controlled migration diagnostic instead of silently reinterpreting a
project. Git source dependencies and a lockfile, when they arrive, will extend the dependency
surface under this same contract.

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
