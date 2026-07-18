# Aster examples

Install the local CLI by following [Getting started](../docs/getting-started.md) before running these
programs.

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

## Focused examples

The remaining valid programs explore narrower behavior used during compiler development:

- `arrays`, `array_initialized`, and `array_structs` cover distinct allocation and value cases.
- `integer_widths`, `numeric_types`, and `expressions` exercise numeric rules and evaluation order.
- `classes_counter`, `class_composition`, `properties_and_equality`, and `structs` show different
  value/reference combinations.
- `void_main` shows a `public static void Main()` entry point that produces no value, plus an
  instance created explicitly with `new` whose methods use fields and sibling methods without
  qualifying the receiver.
- `string_interpolation` shows `$"...{expr}..."` building a `string` from an instance method's
  result.
- `generic_types`, `enum_payloads`, and the result-propagation variants cover concrete layouts that
  reach the JIT.
- the `multifile*` directories are integration programs for project linking and dispatch.

Some focused files expose a namespace-level function for explicit execution, for example:

```console
aster run examples/expressions.aster --function Run
```

`decimal_frontend.aster` is intentionally check-only: it documents that decimal syntax and type
checking exist while the JIT representation does not.
