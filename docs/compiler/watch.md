# `aster watch`

```
aster watch <FILE> [--function <NAME>]
```

`FILE` is the root source. The watcher tracks it, the nearest `Aster.toml`, and every successfully
loaded transitive namespace dependency; changing any of those files rebuilds the root project.

After every stable change, it recompiles and runs the manifest or conventional `Main` entry through
the same pipeline as `aster run`. `--function` selects an explicit public zero-parameter root
namespace-level function instead.

Behavior:

- Polls file metadata every 150 ms; a rebuild fires only after two consecutive identical
  observations of a changed file, so one editor save produces one rebuild (debounce).
- Compile errors are printed and the watcher keeps running; when a later save compiles again
  it prints `[watch] compilation succeeded again`.
- Each successful run prints the Aster frontend time and JIT+execution time separately
  (Cargo build time is not part of either) plus the function result.
- Every rebuild uses a fresh JIT session and ExecutionContext. The previous session is freed after
  its result is copied out — modules, code pages, arrays, objects, and dynamically concatenated
  string data do not leak or dangle.
- The terminal is never cleared. Stop with `Ctrl+C`.

This is **recompile and restart**, not hot reload: no program state survives a rebuild. See
`docs/compiler/hot-reload-foundation.md` for the future hot reload design.

When developing the compiler without installing the binary, see [Development](development.md) for
the equivalent Cargo workflow.
