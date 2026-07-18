# Monomorphization

Parsed generic declarations are templates. After namespace linking and before ordinary semantic
validation, the compiler discovers concrete uses and substitutes their type parameters.

Function specializations are cached by linked function name and ordered concrete arguments. Type
specializations are cached by a structural key containing the linked template name plus nested type
arguments and array shape. The cache entry is installed before the body is visited, so recursion
that reuses the same closed type terminates. Recursion that continually grows, such as
`Grow<T>` containing `Grow<Grow<T>>`, receives a controlled diagnostic.

Each specialized class, struct, interface, or enum becomes an ordinary nominal AST declaration with
concrete fields and signatures. Semantic member identities include their concrete owner, and
expression-resolution metadata also includes its specialized callable context; cloned source spans
therefore cannot make `Box<int>.Get` collide with `Box<string>.Get`.

Enum payloads are substituted before layout and exhaustive-switch validation. HIR assigns normal
`SymbolId` values to the generated declarations. MIR receives concrete layouts, calls, enum cases,
and interface tables. Cranelift has no generic ABI and never receives `T`, boxing metadata,
reflection dictionaries, source paths, or AST nodes.

The current pass validates instantiated templates. Constraints and a separate parametric template
checker are future work; operations unsupported by a concrete specialization still produce normal
semantic diagnostics before HIR.
