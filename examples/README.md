# ASTER examples

Install ASTER by following [Getting started](../docs/getting-started.md). The examples are checked
into the compiler repository, so run the commands below from the repository root.

## Start with complete programs

1. **Hello** prints a message and returns a visible result.

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

4. **Enums** uses an exhaustive `switch`.

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

9. **Language ergonomics** combines contextual construction, expression-bodied methods,
   `foreach (var ...)`, checked list indexing, named arguments, and defaults.

   ```console
   aster run examples/language_ergonomics.aster
   ```

## Multifile projects

Manifest-based projects can be checked and run from their own directory without passing a source
file:

```console
cd examples/hello_app
aster check
aster run
```

Other project examples include:

- `namespaces` for folder namespaces, `namespace`, and `using`;
- `option_result` and `result_propagation_multifile` for errors across files;
- `generic_types_multifile` for concrete generic specialization;
- `path_dependency` for two local packages linked by a `[dependencies]` path;
- `file-indexer` for filesystem, collections, and reporting.

## Focused compiler examples

The remaining programs isolate narrower behavior:

- `arrays`, `array_initialized`, and `array_structs` cover allocation and value cases.
- `integer_widths`, `numeric_types`, and `expressions` exercise numeric rules and evaluation order.
- `classes_counter`, `class_composition`, `properties_and_equality`, and `structs` show
  value/reference combinations.
- `string_interpolation` and `strings_basics` exercise UTF-8 text.
- `generic_types`, `enum_payloads`, and the result-propagation variants cover concrete layouts.
- the `multifile*` directories serve as project-linking integration programs.

Some files expose a public namespace-level function instead of an application entry:

```console
aster run examples/expressions.aster --function Run
```

`decimal_frontend.aster` is a negative compiler fixture. Decimal syntax is parsed and typed, but the
executable subset rejects decimal layout before HIR/MIR execution.
