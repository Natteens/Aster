# ECS research

An Entity-Component-System (ECS) is not part of the ASTER language, runtime, or standard library.
No implementation is scheduled.

An early frontend experiment reserved `component`, `system`, and `read`/`write` access syntax. It
never reached executable HIR, MIR, the JIT, or a runtime scheduler, and was removed. Those words are
ordinary identifiers today. The current `foreach` statement is unrelated: it is compiler-known
iteration over arrays, `List<T>`, and strings.

If an ECS is developed later, the starting assumption is an optional library, framework, or engine
package:

- no ECS concept defines `Program.Main` or an application lifecycle;
- normal ASTER programs remain independent of ECS or engine packages;
- queries, scheduling, resources, events, and conflict analysis need an explicit public design;
- compiler integration would require a demonstrated technical need that an ordinary library cannot
  meet.

This note preserves the boundary for future research; it is not an accepted feature proposal.
