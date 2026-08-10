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

The current pass validates instantiated templates. Operations unsupported by a concrete
specialization still produce normal semantic diagnostics before HIR.

## Interface constraints

A type parameter may carry interface-only `where` constraints. They are stored as ordinary
`TypeRef`s on `TypeParameter`, so the shared mutable visitor lets namespace linking rewrite them to
linked nominal names with no second resolver.

Well-formedness — unknown constraint type, non-interface constraint, duplicate constraint, and
unsupported generic interface constraint — is checked once per template, alongside the other
template rules. Those rules iterate hash maps, so their diagnostics are collected and sorted by
source span before emission to keep failures deterministic. Malformed clause syntax, a clause
naming an unknown type parameter, and a duplicate clause for one parameter belong to the parser.

Satisfaction is proven inside function and type specialization, after the concrete arguments are
known but before the cache is consulted and before a clone is generated. Placing it there rather
than at surface call sites means explicit arguments, inferred arguments, nested requests, eager
internal requests, and repeated bad request sites are all covered, and a satisfied entry can never
silence a later unsatisfied one.

The relation is nominal and deliberately narrow: an argument satisfies an interface when it is that
interface, or when it is a class whose linked declaration lists it. The generic layer keeps only
that minimum inventory — declaration kinds plus class interface lists, including the lists carried
by generated class specializations. It performs no structural member scanning; semantic analysis
remains the sole authority for whether a class implements what it declares.

Constraints live on the type-parameter list, which specialization already clears, so they disappear
with it. HIR, MIR, backend validation, Cranelift, and the runtime have no notion of a constraint,
and adding one to a program changes no generated instruction.
