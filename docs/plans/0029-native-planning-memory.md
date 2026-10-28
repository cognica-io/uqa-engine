# Native planning memory

## Scope and delivery

This group resolves [#1292](https://github.com/cognica-io/uqa-engine/issues/1292) and [#1293](https://github.com/cognica-io/uqa-engine/issues/1293), starting from main `6d29561518909c8b478a07b39c6b421fdb3a4605`. Request admission, allocation lifetime and EXPLAIN observation share the same owners and belong in one implementation PR. The separate statement-analysis, historical-latency and measurement-attribution requirements in #1117/#1108/#653 remain open. The [issue inventory](0024-open-issue-delivery.md) retains the complete delivery queue.

The PR remains draft until the whole correction batch, local verification, review and task records are complete. No intermediate push manually starts, reruns or cancels CI. Completion requires actual final-source Linux and macOS bounded-work, memory and exact-output results, merge and issue closure. This document records work still required; no unchecked entry is a delivered behavior.

## Ownership and allocation inventory

Core owns reservations and containers. SQL owns executable-plan and scalar storage, copying, binding and typed result contracts. Planner owns rewrites, cardinality/cost calculations, join enumeration and EXPLAIN rendering. Execution owns statement preparation and diagnostics sequencing; Engine supplies request/session adapters. The existing Cargo features and dependency policy allow these directions without adding a dependency or moving algorithms into Engine.

| Owner and path | Escaping allocation or lifetime | Required correction |
| --- | --- | --- |
| SQL `plan/copy`, `scalar` copy interfaces | Existing controlled copies return immutable produced values; ordinary `Clone` callers bypass them | Preserve exact pre-admission and immutable shared descriptors; introduce only owner operations that retain the destination leases through subsequent transformations |
| Planner `statement_planning/executable` | Cached analysis uses `cloned()`; query optimization clones and boxes the whole query | Borrow or consume existing plans where possible; otherwise copy through SQL's admitted owner operation |
| Planner `optimizer` traversal and transformations | Derived/CTE projection, pull-up, predicate rewrite, scalar folding, routine inlining and join rewriting allocate replacement plans and scratch | Admit each destination before construction; free discarded storage before releasing its lease; preserve lazy constants, selected routine identities, diagnostics and traversal order |
| Planner `join_enumerator` | Candidate costing recursively clones complete subtrees, allocates edge lists, connected sets, layers and star-state buffers | Retain copy-only decisions keyed by relation subsets, cost borrowed edges, admit memo/layer/state buffers and materialize only the selected tree; preserve the existing search order and cost rules |
| Planner `join_order`, `optimizer/join_reorder` | Graphs, aliases, statistics, atoms and predicates are copied; final trees copy their selected children again | Borrow metadata, move selected owned subtrees and admit unavoidable graph/output storage under the same request |
| Engine `capabilities/statements` | EXPLAIN passes `body.clone()` into Planner | Pass the request context and borrowed input to the owning planning operation |
| Execution `statement/plan_executor/explain` | Cursor, CTAS and materialized-view paths clone query bodies; prepared EXPLAIN has an additional selection boundary | Preserve utility/prepared scheduling while retaining the selected plan and its admission through observation and execution |
| SQL `result/explain`, Planner `explain/planning` | Planning metrics carry I/O only; MEMORY is parsed without output | Add typed owner measurements and TEXT/JSON/XML/YAML rendering independent of buffer and timing visibility |

This inventory is an implementation checklist, not permission to account after allocation. Process allocator totals, guessed sizes, request-wide peaks and disabled optimizer passes cannot substitute for ownership. Shared immutable metadata remains with its existing owner; copied destinations require independent leases. Dependency sources, logging and the C API are outside this group.

## Join search representation and preservation

For a valid join graph with at most 64 relations, let a subset be its existing `u64` relation mask. A retained decision consists of that mask, estimated rows, cost, child subset masks, selected edge and physical operator kind. These fields contain no separately allocated payload. Memo storage admits its exact nodes, temporary layers admit vector capacity, and the selected output admits each boxed node before construction. Removed scratch releases its own reservations; output reservations survive until the produced tree is dropped.

The representation change introduces no algebraic operator. Define expansion of a leaf decision as its relation scan, and expansion of a binary decision as the existing join applied to the expansions of its children. Every child is a strict subset of its parent, so expansion terminates. In exact enumeration, all smaller subsets are settled before a larger subset is visited. In greedy search, a consumed child subset is never rewritten. In the star search, selected prefixes are reconstructed from the completed predecessor table. Thus each retained child reference denotes the same settled child that the earlier implementation copied.

Induction on subset cardinality proves representation preservation: leaf rows/costs and relation masks are identical; a binary candidate uses the same ordered operands, ordered edge selectivities, physical cost function and floating-point operation order, so its rows and cost are identical. Preserving candidate visitation order and strict cost comparison preserves ties and the selected decision. Expanding that decision therefore yields the same ordered tree, including disconnected-component cross joins and their stable cardinality order. This claim does not assume join commutativity for volatile SQL expressions: eligibility and predicate attachment remain in their existing SQL/Planner owners and are unchanged by decision storage.

Successful admission preserves this observation exactly. A failed reservation or either cancellation token terminates before constructing the rejected allocation; RAII drops all local containers and output nodes with their leases. Failure publishes no plan. This proof obligation must be checked against the completed implementation and its exact/greedy/star/disconnected tests before the corresponding task is marked complete.

## EXPLAIN measurement boundary

The independent contract is [PostgreSQL 18 EXPLAIN](https://www.postgresql.org/docs/18/sql-explain.html) and its [planner measurement boundary](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/commands/explain.c). Capture UQA-owned live used bytes and allocated capacity after the selected plan has been produced and before executing it. Released scratch is not live memory; retained capacity and separately owned plan payloads must remain attributable. Parsing, unrelated session caches and execution output are not a substitute measurement. Compare field visibility, units and option behavior against independent PostgreSQL captures, never allocator-dependent byte equality. Prepared cache reuse and supported utility paths need explicit observations without a second execution.

## Live requirements

The first local correction replaces DPccp candidate tree copies with copy-only subset decisions. Exact, star and greedy searches now admit actual memo/state/layer storage, materialize the selected tree once and retain only its boxed-node reservations after return. The ordinary interface uses the same search; native caller propagation remains unfinished. The eight-relation dense regression failed before the change with 56,828 allocations / 4,693,440 total bytes and now uses 552 allocations / 59,312 total bytes. These are deterministic owner-allocation observations, not elapsed-time or historical TPC-H acceptance. A separate 240-graph comparison against the original implementation preserves every selected mask, edge, operator, child orientation and floating-point bit pattern across exact, star, greedy and disconnected searches; the permanent exhaustive-tree and tie-order tests provide independent search checks.

Current local evidence: 297 Planner unit tests, 29 Planner integration tests, strict Planner library/test Clippy, workspace dependency policy, Engine capability policy and integration-harness coverage pass. Eight new owner checks are included in the automatic deterministic regression inventory. Native SQL/prepared/EXPLAIN propagation, four-provider integration and actual Linux/macOS qualification remain required; this local result does not complete either issue.

- [ ] Complete the allocation inventory with failing-before owner tests. Join search is the first correction; scalar, query rewrite, binding/cache copies and native adapters remain required.
- [ ] Admit SQL-owned copies and Planner scratch before allocation, preserve cancellation and exact live leases, and release on success, SQL error and unwind.
- [ ] Preserve lazy diagnostics, volatile effects, result values, transaction/completion behavior, generic/custom selection and cache invalidation.
- [ ] Capture actual native planning used bytes and allocated capacity at the before-execution boundary, including direct, prepared, cache/replan and utility paths.
- [ ] Render all four EXPLAIN formats and verify MEMORY true/false, plain/ANALYZE, SUMMARY, TIMING and BUFFERS against independent PostgreSQL 18 observations.
- [ ] Verify native SQL, prepared and EXPLAIN rejection/recovery on memory, native SQLite, SQLite Key/Value and redb, including persistent reopen where relevant.
- [ ] Complete focused owner tests, deterministic allocation/work limits, ownership/dependency/harness checks and review; then mark the finished draft ready.
- [ ] Verify actual final-source Linux and macOS automatic performance results, merge, close both issues and remove the merged branch/worktree.
