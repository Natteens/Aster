# `aster dump-hir`

`dump-hir` is a development-only inspection command for the Aster frontend:

```powershell
aster dump-hir path\to\file.aster
```

The command reads a `.aster` file, lexes it, parses it, performs semantic validation,
lowers the implemented general-language nodes through the frontend pipeline, and prints its HIR view.
It never executes source code and does not invoke Cranelift or a linker.

Invalid input produces the same positioned diagnostics as `aster check` and exits unsuccessfully.
Warnings, including unreachable-code warnings, are rendered before a successful dump.

Example source:

```aster
public int Add(int left, int right)
{
    return left + right;
}
```

The output is a debug-oriented tree similar to:

```text
Module {
    items: [
        Function(Function {
            symbol: SymbolId(0),
            name: "Add",
            return_type: Int,
            ...
        }),
    ],
}
```

The exact formatting and numeric symbol IDs are development details and are not a stable serialization
format.
