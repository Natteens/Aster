# Internal modules and public namespaces

Aster source code uses [`namespace` and `using`](namespaces.md). The compiler still calls a parsed
file and its linked declarations a *module* or *compilation unit* internally, but that is an
implementation detail rather than source syntax.

If older pre-alpha code contains `module` or `import`, replace them directly:

```aster
namespace app;
using aster.math;
```

The compiler reports a migration diagnostic for the removed words. See
[Namespaces and usings](namespaces.md) for folder inference, visibility, the standard library, and
watch behavior.
