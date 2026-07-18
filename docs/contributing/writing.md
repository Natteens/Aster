# Writing Aster documentation

Write first for the person using the language. Explain why a concept matters, show a small example,
then link to the complete reference or compiler detail.

Use this checklist when reviewing a page:

- Start with the reader's task or question, not an implementation inventory.
- Put an example before exhaustive rules when the document is a guide or tutorial.
- Use concrete names such as `Counter`, `Order`, or `Position`; avoid `Foo`, `Bar`, and `Baz` in
  introductory material.
- Describe implemented behavior in the present tense. Mark proposals and research before discussing
  them, and never present future work as available.
- Prefer direct language to advertising, slogans, milestone reports, or bureaucratic phrases.
- Keep tutorial, guide, reference, and architecture documents distinct. Reference may be dry;
  architecture may assume compiler knowledge.
- Use the established terms: source file, project, namespace, application entry, standard library,
  generic specialization, monomorphization, runtime, JIT, and toolchain.
- Preserve technical precision. Do not simplify away evaluation order, representation, allocation,
  dispatch, or other observable costs.
- Do not explain universal syntax as an identity claim. Aster having `if` or arrays is less useful
  than explaining what its conditions, values, and arrays mean.
- Label excerpts as excerpts. Anything presented as a complete runnable program must compile with
  the documented command.

Keep public documentation in English unless a document is intentionally translated in full. Code,
commands, diagnostics, and technical names remain unchanged.
