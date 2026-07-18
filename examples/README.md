# Aster examples

Install the local CLI from the repository root before running these examples:

```console
cargo install --path crates/aster-cli --locked --force
```

The examples below form a short learning path. Each one uses the standard application entry point,
so no explicit function name is needed.

## Single-file examples

1. **Hello** logs a message and returns a visible result.

   ```console
   aster run examples/hello.aster
   ```

2. **Basics** introduces local variables, a loop, a conditional, and arithmetic.

   ```console
   aster run examples/basics.aster
   ```

3. **Objects** shows a class, constructor, property, and method.

   ```console
   aster run examples/objects.aster
   ```

4. **Enums** uses an exhaustive `switch` over enum cases.

   ```console
   aster run examples/enums.aster
   ```

5. **Generics** specializes generic functions over concrete values and arrays.

   ```console
   aster run examples/generics.aster
   ```

6. **Interfaces** calls a concrete object through an interface.

   ```console
   aster run examples/interfaces.aster
   ```

7. **Option and Result** handles absence and errors with concrete generic enums.

   ```console
   aster run examples/option_result.aster
   ```

8. **Result propagation** demonstrates postfix `?`.

   ```console
   aster run examples/result_propagation.aster
   ```

## Multifile projects

Project commands still receive the root `.aster` source file. The nearest `Aster.toml` establishes
the project root and application entry.

- **Hello app** is the smallest manifest-based multifile application.

  ```console
  aster run examples/hello_app/app/main.aster
  ```

- **Namespaces** demonstrates folders, `namespace`, `using`, and the embedded standard library.

  ```console
  aster run examples/namespaces/app/main.aster
  ```

The remaining files under `examples/` exercise focused compiler and runtime behavior. Some retain
namespace-level functions for targeted development with `--function`; they are regression programs,
not the recommended starting path.
