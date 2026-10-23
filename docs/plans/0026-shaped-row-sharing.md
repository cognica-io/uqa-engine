# Shared row layouts and owned value transfer

The maintainer requested all four follow-ups from the comparison of `jaepil/rapidjson` shaped rows (`fdbe7620342c30f528c5ccdcccc75de9aba48e26`) with UQA main `16533ee6d1aa7636075b97c43ae5a54badcfe1e4`. UQA already shares SQL schemas, physical value fragments and memory document layouts. This work removes repeated metadata and value copies at their owning boundaries while preserving UnifiedPlan, SQL semantics and persistent formats.

## Original task ledger

| Task | Owner and delivery group | Current evidence and remaining work |
| --- | --- | --- |
| [#1254](https://github.com/cognica-io/uqa-engine/issues/1254): move owned physical values into columnar cursor batches | Execution; [PR #1258](https://github.com/cognica-io/uqa-engine/pull/1258) | Consuming transfer moves exclusive selected values, resolves fragment/schema aliases and groups row-local shared owners. All 1,736 non-ignored Execution library tests pass, including seven registered allocation/ownership/output regressions; seven pre-existing ignored tests are not claimed. Public cursor acceptance, final review, platform qualification and merge remain. |
| [#1255](https://github.com/cognica-io/uqa-engine/issues/1255): decode persistent projections directly into positional values | Storage/provider adapters; shared storage PR with #1256 | Selected primitive fields still allocate names and a Document map. Native SQLite cache hits already avoid decoding. Direct decoding, SQLite/KeyValue integration, validation/resource/lifecycle tests and qualification remain. |
| [#1256](https://github.com/cognica-io/uqa-engine/issues/1256): reuse indexed layouts and cursor projection mappings | Storage; shared storage PR with #1255 | Layout search and projection construction repeat metadata work. Owner-scoped indexing/reuse, heterogeneous-layout and paged-read tests and qualification remain. |
| [#1257](https://github.com/cognica-io/uqa-engine/issues/1257): share immutable composite descriptors | Core with SQL/Execution construction adapters; independent PR | Record field names and anonymous Row descriptors can repeat per value. Shared representation, construction reuse, type/lazy-read/serialization/persistence acceptance and qualification remain. |

- [ ] Deliver #1254 with final-source qualification and issue closure.
- [ ] Deliver #1255 with final-source qualification and issue closure.
- [ ] Deliver #1256 with final-source qualification and issue closure.
- [ ] Deliver #1257 with final-source qualification and issue closure.

The first PR has no implementation dependency. Later groups preserve this ledger without treating the first delivery as completion of all four tasks. General string interning and a C++ dependency are outside these four findings. The unrelated open-issue delivery ledger remains separate.

## Ownership and preservation

Execution owns materialization and cursor transfer; SQL owns names, ambiguity, types and slot mappings. Storage owns projection and decoding; providers supply snapshots, bytes and compound-value hydration. Core owns composite descriptors without a SQL/catalog dependency. Engine supplies session, transaction and retained-resource adapters.

Preserve output order, duplicate labels/slots, missing fields versus NULL, present all-NULL records versus absent outer-join rows, types/collations, diagnostics/evaluation order, cancellation, memory accounting and snapshot ownership. Storage changes retain existing bytes and reopen behavior. Composite changes retain descriptor identity across DDL, rollback and delayed reads, including the approved XX001 owned-tuple boundary.

## Owned columnar conversion argument

Let a batch contain ordered rows \(r_0,\ldots,r_{n-1}\), logical-to-physical mapping \(s\), and output-label occurrence resolution \(p_j\). Its transfer observation is \(C_j[i]=r_i[s(p_j)]\), or SQL NULL for an absent position. Consuming conversion must preserve every value and \(n\), including zero-column rows.

An immutable shared owner permits an observationally equal clone. An exclusive owner can move a value on its last selected occurrence; earlier occurrences receive clones. Count occurrences after schema and fragment projections so aliases cannot move a value prematurely. Unselected values are dropped without evaluation. Therefore every \(C_j[i]\), duplicate position, row order and cardinality is preserved. No expression is evaluated and no algebraic operation changes. Shared owners remain unchanged; moved allocations belong to their output columns. Tests cover identity/remapped layouts, shared/projected fragments, duplicate labels/slots, missing positions, empty batches and zero-column rows.

## Verification and delivery

Use focused owner tests and deterministic allocation/work assertions, followed by affected crate and cross-crate acceptance. Register regressions in the existing performance manifest; retain one test executable per crate. Check ownership/dependency policies, formatting and contracts before readiness. Draft pushes collect logical commits; completed ready-for-review source must pass actual Linux/macOS performance results and affected FDW configurations before merge. Record tested revisions and remaining requirements here and in each PR. Lower allocations alone do not establish a latency speedup; timing acceptance requires a controlled host and an independent noise bound.
