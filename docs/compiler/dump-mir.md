# `aster dump-mir`

`dump-mir` is a development-only command that exposes Aster's initial mid-level representation:

```powershell
aster dump-mir path\to\file.aster
```

The command reads and validates the `.aster` source, lowers its general-language HIR to MIR, and prints
the result. It never executes the program and does not invoke Cranelift, machine-code generation, or a
linker. Invalid input is rejected with the normal positioned diagnostics before MIR
is created.

The output is a debug-oriented tree containing typed functions, locals, basic blocks, instructions,
and explicit terminators. For example, an `if` becomes a `Branch` pointing to two `BasicBlockId` values;
loop back-edges and exits become `Goto` terminators.

```text
Function {
    name: "Choose",
    entry: BasicBlockId(0),
    blocks: [
        BasicBlock {
            id: BasicBlockId(0),
            terminator: Branch {
                then_block: BasicBlockId(1),
                else_block: BasicBlockId(2),
                ...
            },
            ...
        },
        ...
    ],
    ...
}
```

The printed format, temporary names, local numbers, and block numbers are internal development details,
not a stable serialization format.
