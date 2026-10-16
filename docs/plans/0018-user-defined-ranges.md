# User-defined ranges and multiranges

This plan preserves original record 65 of the [extension task ledger](0018-extension-progress.md) and the range contract in the [type extension design](../design/data-model-and-type-extensions.md). The completed composite prerequisite does not implement this unit. Keep each implementation boundary in a focused PR, retain all original task records in successors, and record actual verification before closing a boundary.

## Current boundary

SQL's `RangeSubtype` identifies six built-in families. `CanonicalRange` and `CanonicalMultirange` retain admitted transient bounds, while stored built-in range values use text with a declared SQL identity. `expr/range/production.rs` selects canonicalization from the built-in subtype; `RangeComparisonValue` supplies admitted bounds to comparison and exact-key consumers. User-defined range and shell declarations are not implemented by this correction.

[#299](https://github.com/cognica-io/uqa-engine/issues/299), in [PR #916](https://github.com/cognica-io/uqa-engine/pull/916), separately validates default B-tree capability. [#917 input acceptance](0018-range-input-acceptance.md) corrects independently reproduced built-in input differences before the subtype path is generalized. That source is committed and locally verified; its automatic platform acceptance remains required. Neither prerequisite completes record 65.

An independent Docker PostgreSQL 18.4 preparation corpus contains 61 declaration and observation cases, including three fresh-connection observations. It establishes that an integer subtype without a canonical routine keeps its written inclusive/exclusive bounds, enum endpoints retain label identity after rename, shell visibility differs from regtype usability, and renaming a range does not rename its constructor routine. This is preparation evidence, not a complete acceptance matrix; commit the expanded fixtures with the owner tests that consume them.

## Ownership and representation

Inspect the owning manifests, enabled features, dependency policy and existing consumers before each implementation boundary. SQL owns declared range identity, syntax, selected endpoint semantics, input/output, validation order and admitted operations. Core owns any shared value carrier and its structural storage primitives without depending on SQL catalogs. Execution owns definition transactions, support-function invocation, permissions, dependency publication and catalog restoration. Engine supplies existing state, session, transaction and retained-resource adapters. Provider codecs retain opaque value state through their existing controlled read/write contracts.

Keep immutable range identity separate from the endpoint type, selected collation/operator family and optional canonical routine. Two ranges over the same integer subtype are distinct types. A user-defined integer range does not automatically select the built-in integer canonicalizer. Generated arrays, the associated multirange and constructor routines have their own catalog identities and dependencies; current spelling cannot substitute for those identities.

Custom values must retain typed endpoints once. Re-reading stored endpoint text would repeat domain input and lose enum identity after a label rename. `SQLValueCatalog` observes already admitted values; it must not gain a callback that executes arbitrary type input. Selected operations borrow retained metadata, and callable support routines execute only at the boundary that owns their effects. Output may resolve current enum labels without changing stored equality or ordering.

Exact/hash keys must use the selected endpoint equality contract, including collation and nested types. An available byte encoding is not evidence that it agrees with a selected comparator. Preserve the existing built-in representation and keys unless a separately reviewed migration is necessary; any new durable carrier requires provider format/version checks, old-owner rejection, bounded decoding and failure-atomic restoration.

## Review boundaries and exit evidence

| Boundary | Required implementation | Evidence before delivery |
| --- | --- | --- |
| Selected bound semantics | Separate range identity, subtype observation and optional canonicalization in SQL's actual production/comparison path. Retain native adapters; do not add an unused descriptor. | Existing built-in values, diagnostics, comparisons, empty/infinite bounds, exact keys, memory and cancellation remain correct. |
| Typed endpoint retention | Add the custom carrier and admitted codec interfaces needed for immutable endpoint identity. Integrate nested arrays/domains/composites and consuming output/comparison paths before exposing custom DDL. | Enum rename, domain input evaluation count, retained values, selected equality/hash keys and controlled physical reads; provider migration evidence if the durable format changes. |
| Shell and range publication | Lower shell/range declarations in SQL and publish completed range, array, multirange, constructor, catalog and dependency objects atomically in Execution. Validate subtype, operator class, collation and support signatures in PostgreSQL order. | Independent successful and failed declarations, name conflicts, shell completion, catalog rows, ownership, transaction/savepoint rollback and peer refresh. Parser acceptance alone is insufficient. |
| Operations and observing consumers | Resolve constructors independently of current type spelling; apply selected canonical/support operations and retain type identity through range/multirange operators, aggregation, nested containers, grouping, joins and indexes. | Independent enum/text/numeric/temporal subtype cases, selected collation, support-function failures, comparison transitivity and equality agreement with exact/hash/index consumers; actual spill and bounded work. |
| Lifecycle and public acceptance | Complete ALTER/rename/schema movement, DROP/dependencies, privileges, prepared/held values and provider restoration. Update the public manual and execute actual binding artifacts. | Four-provider references, actual persistent close/reopen, concurrent histories, Rust/Python/Node.js/WASM observations and exact-source Linux/macOS automatic work, memory and output reports. |

Refine a boundary before implementation if its owner dependencies prevent a coherent change. Do not publish incomplete aliases as range types, move algorithms into Engine or remove required cases to shrink a PR. The full record remains open until every original requirement and observing consumer is verified.

## Proof obligations

For each selected range identity $r$, define its endpoint domain $T_r$, selected comparison $\leq_r$, empty/unbounded values, inclusivity flags and optional canonical map $C_r$. Prove that native adapters select their existing operations and values. For custom operations, state the PostgreSQL invocation and error behavior rather than assuming an arbitrary user routine is pure or idempotent. Any optimizer rewrite must state and justify the stronger laws it requires.

Derive range order and multirange normalization from the selected endpoint order and the empty/unbounded rules. Show that observing consumers use the same equality classes and that any exact/hash/index representation preserves them. For serialization $E_r$ and admitted decoding $D_r$, prove that $D_r(E_r(v))$ preserves type and endpoint identities and every supported observation; equality of output text alone is insufficient. Preserve transaction effects, lazy read boundaries, memory ownership and cancellation when applying the existing relational compositions.

Finite PostgreSQL fixtures and resource tests exercise these obligations but do not replace their argument. Keep original records 66–75 and all earlier unfinished requirements in the parent ledger; this plan neither implements package installation nor closes the larger extension project.
