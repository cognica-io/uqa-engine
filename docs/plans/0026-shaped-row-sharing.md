# Shared row layouts and owned value transfer

The maintainer requested all four follow-ups from the comparison of `jaepil/rapidjson` shaped rows (`fdbe7620342c30f528c5ccdcccc75de9aba48e26`) with UQA main `16533ee6d1aa7636075b97c43ae5a54badcfe1e4`. UQA already shares SQL schemas, physical value fragments and memory document layouts. This work removes repeated metadata and value copies at their owning boundaries while preserving UnifiedPlan, SQL semantics and persistent formats.

## Original task ledger

| Task | Owner and delivery group | Current evidence and remaining work |
| --- | --- | --- |
| [#1254](https://github.com/cognica-io/uqa-engine/issues/1254): move owned physical values into columnar cursor batches | Execution; [PR #1258](https://github.com/cognica-io/uqa-engine/pull/1258) | Consuming transfer moves exclusive selected values, resolves fragment/schema aliases and groups row-local shared owners. All 1,736 non-ignored Execution library tests pass, including seven registered allocation/ownership/output regressions; seven pre-existing ignored tests are not claimed. Public cursor acceptance, final review, platform qualification and merge remain. |
| [#1255](https://github.com/cognica-io/uqa-engine/issues/1255): decode persistent projections directly into positional values | Storage/provider adapters; [draft PR #1259](https://github.com/cognica-io/uqa-engine/pull/1259) with #1256 | Shared legacy decoding emits positional values without result field names, preserves missing/NULL and shares duplicate requested fields. All 958 Storage library tests pass, including error/normalization and bounded-allocation checks. Modern KeyValue formats, provider integration, provider lifecycle acceptance and final qualification remain. |
| [#1256](https://github.com/cognica-io/uqa-engine/issues/1256): reuse indexed layouts and cursor projection mappings | Storage; [draft PR #1259](https://github.com/cognica-io/uqa-engine/pull/1259) with #1255 | Indexed exact layout discovery and immutable shared field names pass all 27 memory document-store tests, including 2,048-layout work counts, collision validation and independent snapshot additions. Cursor-scoped projection reuse, its acceptance and final qualification remain. |
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

## Positional decoding and layout preservation argument

Let \(D\) be the historical field decoder after its ordinary validation, duplicate-key normalization and typed-value conversion, and let \(S=(s_0,\ldots,s_{m-1})\) be the requested names. The positional representation retains one value for each selected distinct name and a mapping \(P_i\) to that value, or an absent sentinel when \(s_i\notin D\). Consequently reading position \(i\) returns exactly \(D[s_i]\) when present; an explicit NULL has a real slot and remains distinguishable from absence. Repeated names share a slot without changing positional observations. The legacy fallback retains the original complete validation and sorted surviving-member conversion order, even for unselected fields; the canonical primitive path retains its historical skipped-string and checked-number rules. Neither path emits a root field-name map. A failed decode publishes no row, and the producer's allowance remains attached to all selected buffers and payloads.

Memory layout lookup hashes the exact ordered names, then checks actual names and width in the selected bucket. Hash equality alone never establishes identity, so collisions cannot combine different layouts. Immutable field-name arrays can be shared across cloned indexes; each index keeps its own ID mapping and additions. Values remain ordered by the same BTreeMap iteration used by the prior implementation. Thus identity selection, field lookup and snapshot observations are unchanged while lookup examines only candidate layouts.

## Verification and delivery

The storage delivery initially uses the Execution PR as its base only to preserve the common task ledger and completed transfer boundary; its diff is limited to Storage and projection adapters. Once #1258 merges, retarget it to main and verify the final source. Legacy duplicate-key normalization and complete numeric/tag validation must remain ordered before positional emission. Field absence must remain distinct from an explicit NULL, and duplicate requested fields must retain distinct positions without repeating decoding. Memory layout discovery must use exact ordered field identities and preserve COW snapshot ownership; compiled projection lifetime must follow its retained read owner.

Use focused owner tests and deterministic allocation/work assertions, followed by affected crate and cross-crate acceptance. Register regressions in the existing performance manifest; retain one test executable per crate. Check ownership/dependency policies, formatting and contracts before readiness. Draft pushes collect logical commits; completed ready-for-review source must pass actual Linux/macOS performance results and affected FDW configurations before merge. Record tested revisions and remaining requirements here and in each PR. Lower allocations alone do not establish a latency speedup; timing acceptance requires a controlled host and an independent noise bound.
