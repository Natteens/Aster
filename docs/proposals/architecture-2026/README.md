# ASTER architecture audit and master plan (revision 3: decisions closed)

> **Status: PROPOSED. Not an approved architecture.** Nothing in this directory is accepted
> language, compiler, or runtime behavior. These documents do not replace the specification, the
> reference, or the public roadmap. Every new decision stays `PROPOSED` until the maintainer reviews
> it.

| | |
| --- | --- |
| Baseline | `origin/main` at `8b5466b` (after PR #85), workspace version 0.71.0 |
| Audit date | 2026-11-10 |
| Method | Static audit. Canonical documents read in full; source inspected directly and through three narrow-scope auditing agents. No build, test, or benchmark was run. |
| Not done | No code, public documentation, version, or release was changed. Nothing was committed. |

## Documents

| File | Deliverable | Content |
| --- | --- | --- |
| [01-repository-map.md](01-repository-map.md) | A | How ASTER works today, with files, contracts, tests, limits |
| [02-master-plan.md](02-master-plan.md) | B | Invariants, contracts, and the recommended design per area |
| [03-decision-register.md](03-decision-register.md) | C | Every decision with a stable ID, review class, evidence, alternatives, dependencies |
| [04-roadmap.md](04-roadmap.md) | D | Phases and pull requests in dependency order, each with its gate |
| [05-maintainer-report.md](05-maintainer-report.md) | E | The summary for review (revision 2) |
| [06-decision-closure.md](06-decision-closure.md) | F | Revision 3: all pending decisions closed under the maintainer's delegation |
| [07-tasks.md](07-tasks.md) | G | Revision 3: executable tasks with status |

## Revision 2: what changed after the first review

Scope was not widened. IDs are preserved; the additions are `CONC-005b` (split out as deferred) and
the question IDs `Q-…`.

| Area | Correction |
| --- | --- |
| MEM-001, MEM-003 | Storage classes fixed at allocation with no run-time promotion; heap invariants H1 to H5; owned and borrowed rules, including the guarantor rule for arguments; obligations U1 to U8 against use after free, each with an independent check |
| MEM-005 | Reclamation, drop glue, and `Dispose` separated. **Losing the last reference no longer calls `Dispose`** |
| MEM-002, OBJ-001 | The count word is presented as an amendment to B1 §4.1, not as a settled layout. H8, H12, and H16 compared with computed sizes. The approved descriptor pointer at offset zero is preserved in every proposed variant |
| MEM-004 | Exact behavior of cycles, weak references, leak tools, and the limits of deterministic reclamation |
| CONC-001 | Turns and reentrancy, blocking table, tasks without `await`, fairness, domain lifetime, failure, transfer, FFI callbacks |
| CONC-005 | Moving class graphs withdrawn to deferred; class references stay non-transferable, as B1 and the reference require |
| HOT-001 to HOT-004 | Slots keyed by signature, classes of change, safe points, the four conditions for a type change, every kind of reference into old code, the unload rule |
| PERF-005 | Deterministic invariants (may gate CI) separated from performance metrics (advisory only) |
| Goals | Numeric targets withdrawn. Properties that hold by construction are separated from quantities that need a baseline |
| B1 cross-check | Eighteen relations tabulated (master plan §6.3). No approved B1 decision is contradicted |
| Breaking changes | All held as pending maintainer decisions; nothing ungated depends on them |
| BENCH-001 | Each workload checked against today's syntax and harness; proxies or "not measurable" stated for the rest |

## Evidence labels

- **[V]** read directly in the source or document by the lead auditor.
- **[A]** reported by an auditing agent with a file and line; not re-derived unless also marked [V].
- **UNVERIFIED** not confirmed by anyone in this audit.

## Status vocabulary

For the current implementation: `IMPLEMENTED`, `GATED`, `PARTIAL`, `PROPOSED` (document only),
`RESEARCH`, `DEFERRED`, `UNVERIFIED`.

For proposals (review class): `READY`, `EXPERIMENT`, `MAINTAINER`, `DEFERRED`; defined in the
decision register.

No performance number in these documents was measured by this audit. Numbers quoted from
`docs/research/aarm.md` are the repository's own same-machine measurements. Object sizes in the
header comparison are computed from B1's layout rules, not measured.
