# AI contribution policy

AI tools may be used to design, implement, test, document, and review contributions to UQA Engine. Contributors remain responsible for understanding and defending every submitted change, proof, and verification result. AI output does not establish correctness. The [contributor licensing policy](CONTRIBUTOR_POLICY.md) continues to apply.

Disclosure of AI use is optional. Contributions do not need AI-use declarations or labels in code, documentation, commit messages, or pull requests.

## Preserve the behavioral contract

Contributions must match the project's target PostgreSQL version, currently PostgreSQL 18, and preserve UQA's established semantics. PostgreSQL compatibility includes values, types, NULL behavior, comparisons, diagnostics, validation order, transaction effects, and persistent state. Use an independent instance of the target version or independently captured reference results to verify affected behavior; expectations generated from UQA's implementation or an AI answer are not an independent oracle.

Preserve the UQA contracts defined in the [manual](docs/manual/README.md) and the [implementation plan's theoretical anchors](docs/plans/0001-uqa-engine-implementation-plan.md#2-theoretical-anchors), including the distinctions among document support, payloads, value relations, graph carriers, scores, and ranked results. Passing existing tests is insufficient when their expectations contradict the intended contract. A PostgreSQL mismatch is a bug to correct, not an existing UQA semantic to preserve; describe the correction and add independent regression evidence while preserving the remaining UQA contract.

## Prove feature additions

Every product feature addition must include a written, reviewable proof that it satisfies the relevant UQA algebraic structure and preserves the meaning of existing compositions. State the definitions and assumptions, identify the applicable laws, and derive the claimed result. A complete mathematical argument or a checked formal proof is required; examples, generated explanations, benchmark results, and finite property-test runs do not replace it.

The proof must cover:

- The affected carrier, domain and codomain, operations, observable results, and any state effects or preconditions.
- The laws that actually apply to that carrier, such as closure, identities, associativity, order, or distributivity, together with the argument that the extension preserves them.
- Preservation of existing behavior and composition, including the conditions under which any optimizer rewrite is valid and the correspondence between the mathematical definitions and implementation.
- Any approximation or numeric assumptions, with explicit bounds and a justification consistent with the existing contract.

Use UQA's actual structures. For example, equality of document support does not establish equality of decorated postings or ranked results. `Payload` is not a semiring, and its collision merge does not generally satisfy full-value commutativity or idempotence. Do not assert a law merely because an operator has a familiar name.

A feature that introduces no new algebraic operator must still show that its integration preserves the existing operations and observations it exposes. Keep the proof with the relevant design documentation and link it from the pull request. Property, differential, and regression tests must exercise the stated obligations and implementation boundaries in the owning crates. These correctness and proof requirements apply to contributions regardless of how they were authored.

## Maintainer judgment

Maintainers may reject a contribution on code or design taste alone, including abstraction choices, API shape, organization, naming, complexity, or fit with the project's direction. An objective correctness defect is not required for rejection. Correct behavior, a valid proof, passing CI, or the use of AI does not entitle a contribution to acceptance.
