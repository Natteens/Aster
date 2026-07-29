# GPU and optional engine research

GPU execution is a separate compiler target, not an optimization mode for ordinary ASTER code.
A CPU function may allocate objects, call runtime services, or use control flow that cannot be
translated safely or efficiently to a GPU. The compiler must therefore require an explicit GPU
context and validate a restricted subset before producing shader or compute code.

Possible future paths include explicit shader modules, compute-kernel declarations, and a runtime
integration with `wgpu`. Open questions include address spaces, host/device data transfer,
resource binding, synchronization, supported scalar/vector layouts, diagnostics, and whether a
portable intermediate representation is needed. None of these choices is accepted yet.

A future ASTER engine would also remain optional. It could combine ordinary ASTER code with
`aster.math`, `aster.tasks`, `aster.ecs`, rendering, input, audio, and asset libraries. It must not
introduce a mandatory lifecycle or make ECS/GPU concepts prerequisites for tools, services,
simulations, or other general applications.
