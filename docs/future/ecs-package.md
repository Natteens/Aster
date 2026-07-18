# Future — ECS as an optional package

Status: not part of the language. No compiler support is implemented or promised.

Aster is a native, general-purpose language. An Entity-Component-System (ECS) model
is not required to use it, and never was part of the accepted language design.

## What existed and was removed

An early, incomplete ECS syntax experiment (`component`, `system`, `foreach`, and
`read`/`write` access parameters) was reserved in the lexer and parsed into
dedicated AST nodes. It was never lowered past the frontend: HIR and MIR only
counted these declarations, and the Cranelift backend refused to JIT-execute any
module containing them. The runtime and standard library never implemented ECS
behavior (no scheduler, no queries, no world, no components, no resources, no
events). That experimental syntax has been removed from the lexer, parser, AST,
semantic analysis, HIR, and MIR. `component`, `system`, `read`, and `write` are
now ordinary identifiers; `foreach` is not currently a recognized statement.

## Direction for a future ECS

If an ECS is built for Aster, it should be a **library, framework, or engine
package written using Aster**, not a feature of the core language:

- No ECS keyword, grammar, or compiler-known syntax is promised.
- No ECS concept defines the application entry point, execution lifecycle, or
  frame/update model — `Program.Main` and ordinary function calls remain how
  Aster programs run.
- The language and any future engine built on it are separate projects.
- Special compiler integration for ECS (dedicated syntax, semantic checks, or
  backend support) would only be reconsidered later, and only with a concrete,
  demonstrated technical benefit over an ordinary library implementation.

There is currently no scheduled work toward this. This document exists so a
future ECS proposal has a place to start from, not to declare one accepted.
