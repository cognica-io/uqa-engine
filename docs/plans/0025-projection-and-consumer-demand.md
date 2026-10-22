# Projection and consumer demand

## Original scope

This [draft PR #1247](https://github.com/cognica-io/uqa-engine/pull/1247) is the next independent group in the [112-issue delivery ledger](0024-open-issue-delivery.md), covering [#990](https://github.com/cognica-io/uqa-engine/issues/990), [#1015](https://github.com/cognica-io/uqa-engine/issues/1015) and [#1019](https://github.com/cognica-io/uqa-engine/issues/1019). Preserve every original report and later reference boundary in those issues. The preceding LATERAL group merged in PR #1244 as `f0c2196a0`; this draft is reconciled with that main. Its corrections and qualification do not complete this group.

- [ ] #990: omit unobserved derived/view/folded-CTE outputs before constant simplification while preserving required values, positional types, privileges, set multiplicity, DISTINCT, ordering, volatile effects and set-producing expressions. Cover all six original remaining failures, aggregate FILTER/ORDER BY, whole-row references, joined/lateral inputs, selected routine identity, CTE materialization and UNION ALL pull-up barriers. The opposite whole-row defect must also be fixed; merely discarding more expressions is insufficient.
- [ ] #1015: apply eligible outer predicates before evaluating derived-table outputs they discard, including views and folded CTEs. Preserve volatile, set-returning, LIMIT/OFFSET, materialization and other non-inlinable boundaries as PostgreSQL evaluates them.
- [ ] #1019: expose successful rows before a later failing row when LIMIT, cursors, EXISTS or another early-stopping consumer does not request that failure. Cover filters, joins, lateral inputs, ordinary and set-returning projections and every other batching path. Preserve side-effect counts and order, not only the returned error.

## Baseline and independent evidence

The 2026-10-23 main `9eb335c2399323710c9e6d62ed0e2310c3db9ca8` has identical crate sources and Cargo manifests/lockfile to `v0.6.1`. Baseline replay uses the official Python `uqa==0.6.1` package. It compares rows, labels and primary messages only, so it is diagnostic evidence rather than complete typed/provider acceptance.

| Existing independent PostgreSQL 18.4 matrix | Baseline matches | Remaining evidence |
| --- | --- | --- |
| Original #990 cases | 48 / 54 | All six original differences still reproduce |
| Additional output-demand boundaries | 34 / 47 | Whole-row, joins, lateral sources, aggregate filters, folded CTE and SRF demand remain |
| UNION planning boundaries | 2 / 10 | Unconditional branches and branches behind predicate/OFFSET/LIMIT boundaries require different simplification behavior |
| Routine identity and COUNT arguments | 40 / 48 | Qualified stars, whole rows, selected user routines and shadowed names remain |
| Fresh predicate/consumer matrix | 34 / 51 | Seventeen differences across sources, derived predicates, side effects and cursor demand |

The fresh matrix is independently captured from a disposable Docker PostgreSQL 18.4 database in `tests/parity/pg18/projection_consumer_oracle.input.json` and its expected file. It includes 52 observations counting the server-version control. Its successful controls are significant: the simple table LIMIT filter, direct projection LIMIT, current view rewrite, materialized/limited derived barriers, LIMIT 0 and several simple-table volatile cases already agree. Joins and EXISTS still advance a sequence beyond the consumer's first result; a filtered derived output can return sequence value 2 where PostgreSQL returns 1. A deferred-error buffer alone cannot correct that observation.

Raw baseline reports and diagnostic captures remain outside the repository. The original #990 independent matrices will be incorporated with complete typed/error expectations as the owner tests are implemented; none of their boundaries is waived by the smaller fresh matrix.

## Current implementation checkpoint

The first Planner correction shares input-value traversal between direct derived sources and folded CTEs. Selected COUNT(*) consumes rows without demanding every output; FILTER/ORDER BY remain visible, and whole-row references retain all fields unless a real column shadows the relation name. All 195 Planner unit tests, strict Planner Clippy, formatting and ownership/dependency/file-length checks pass, including the new owner controls. Whole-row shadowing is checked on demand without copying a full output-name list. Joined/set-member propagation, predicate placement, consumer backpressure and final typed/provider acceptance remain required.

The next logical correction resolves a bare function star without allocating or inferring a value argument, then validates aggregate syntax against the selected routine. Its independent PostgreSQL fixture exposed a second identity failure: SQL lowering and the optimizer recomputed aggregate kind from the written name alone, turning `pg_catalog.count` into scalar execution and potentially selecting a user `count()` from the search path. Both now use SQL's selected-identity classifier; Execution uses the system aggregate's local kernel name without changing registered routine identity or allocating a name per input row. All 2,204 SQL and 196 Planner unit tests, 67 focused Execution aggregate/window tests and 112 Engine integration tests pass locally. The Engine run includes all four providers, persistent reopen, existing aggregate/ordered-call/optimizer cases and the executable manual. Strict Clippy for all four crates, formatting, dependency/capability/harness/file-length policies and the 1,584-check/2,210-case regression inventory pass. This is a locally verified logical commit in the draft, not final group qualification.

`function_star_oracle` contains 28 independent PostgreSQL 18.4 observations, with 24 replayed after reopening each persistent provider. It covers scalar-star diagnostics, ordinary/whole-row counts, selected scalar shadowing, qualified numeric/collection aggregates, FILTER, DISTINCT, grouped/window/ordered-set calls and empty input. This focused evidence does not replace the original demand, predicate and consumer matrices or complete any of the three issues.

Concurrent main monitoring found [#1248](https://github.com/cognica-io/uqa-engine/issues/1248): the automatic sampled-statistics test fails on Linux with one completed publication but without the expected persisted count. Main's Linux/macOS performance artifacts pass their exact source and case checks; that evidence does not waive the separate workspace failure. Investigation and correction are required before this delivery's final qualification. No workflow rerun or timeout increase is used.

## Owning interfaces

SQL owns bound names, selected routine identity, typed scalar traversal, source descriptors and expression semantics. Reuse `ScalarExpr::try_visit` and the established SQL identity/type interfaces; do not create another exhaustive expression walker or classify a selected aggregate by a display name alone.

Planner owns output liveness, derived/CTE/set-member demand propagation, simplification order and legal predicate movement. Extend `optimizer/derived_projection.rs`, `cte_projection.rs` and their callers through shared demand analysis, retaining complete result descriptors and privilege observations. PostgreSQL's unconditional constant UNION member differs from a retained subquery behind a predicate, OFFSET or LIMIT; do not erase every unused UNION ALL expression indiscriminately.

Execution owns physical row demand, expression evaluation order, batching, cursor consumption and bounded retained input. Reuse its existing physical operators, row layouts, materialization and spill owners. Inspect every caller before choosing the demand interface; forcing every query to scalar execution or hiding an error after eagerly executing unwanted side effects is not a complete correction. Engine remains a session/catalog/transaction/resource adapter. No new dependency direction or feature toggle is expected; SQL, Planner and Execution manifests and the workspace dependency policy have been inspected.

## Logical commits and verification

- [x] Preserve the original scope, reproduce the current baseline and capture independent predicate/consumer controls.
- [ ] Share selected-routine-aware output demand and correct unused derived/CTE/join/set outputs before folding; add owner tests for positive and negative demand boundaries.
- [ ] Apply eligible derived predicates at the PostgreSQL evaluation boundary; verify volatile/SRF and non-inlinable controls.
- [ ] Implement consumer-driven row/error/effect ordering through affected physical operators, retaining bounded memory and native cursor behavior.
- [ ] Replay the complete independent matrix on memory, native SQLite, SQLite Key/Value and redb, including retained views/routines and reopen where applicable; verify types, SQLSTATE, primary, DETAIL/HINT and sequence effects.
- [ ] Complete focused local owner/integration tests, review, formatting, strict Clippy, dependency/capability/file-length checks and synchronized HISTORY/manual/manifest/task records.
- [ ] Run one completed automatic PR batch and inspect actual Linux/macOS performance work, memory and output results, plus selected FDW/binding checks; merge, close all three fully resolved issues and remove the branch/worktree.

Keep this PR draft while collecting logical commits and local evidence. At most one completed preceding group may be under automatic qualification while this independent draft advances; do not add more delivery PRs to that queue. Merge the preceding qualified group first and reconcile this branch with main before final qualification. Ordinary draft pushes must not start CI, and no manual dispatch, rerun or cancellation is used.
