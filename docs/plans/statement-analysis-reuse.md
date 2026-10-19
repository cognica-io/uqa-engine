# Reuse ordered statement analysis

This bounded correction addresses the duplicate statement-result pass in [#1117](https://github.com/cognica-io/uqa-engine/issues/1117). SQL's input preparation already derives output names, types, collations and anonymous-record fields while checking expressions in PostgreSQL order. The ordinary and procedural statement adapters discarded that schema and analyzed the converted tree again. A local work-count regression reproduces two block analyses for `SELECT id FROM items`; retaining the first result reduces this to one without introducing a cache.

## Ownership and implementation

The existing SQL preparation owner returns its result schema together with input-reuse eligibility and prepared dependencies. Statement adapters consume these values in their original catalog scope after constant conversion, composite-input retention and window normalization complete. EXPLAIN retains its separate body scope. Query-bearing commands still validate their inputs and return command metadata; mutation RETURNING keeps its names, positional types and record descriptors. Runtime expressions are not evaluated by analysis.

The correction adds no dependency or feature and changes no value operator or result carrier. SQL continues to own binding and result descriptions; Engine and Execution receive the same result contract. Both affected crate manifests, the dependency policy, existing preparation paths and the manual ownership boundaries were reviewed before implementation.

## Tasks and verification

- [x] Reproduce repeated block analysis with a deterministic owner test before changing product code.
- [x] Transfer the already-derived result through ordinary and procedural statement analysis.
- [x] Check output names and declared types independently with PostgreSQL 18.4 in Docker.
- [x] Finish local owner, exact TPC-H, procedural and prepared-result validation on the delivery branch.
- [x] Review accounting, error precedence and dependency/ownership checks; update HISTORY and the PR body.
- [x] Verify actual automatic Linux and macOS bounded-work, memory and output results for the delivered source.
- [x] Merge this correction and remove its branch.

Local validation passes 2,120 SQL unit tests, 67 integration tests and 1,469 selected Engine cases, including all 22 TPC-H fixtures. Automatic run 39307635675 passes actual Linux and macOS execution: each report contains 1,143 checks / 1,643 cases and eight process histories, with zero failures, errors or skips. Both downloaded reports independently match source `4645582ad1eb5652df0274139c4dd389fad2e4cb`, tree `6cbb365c0d9ac358376f0f38a52c043287c9af64` and every selected source/case identity. The subsequent main reconciliation changes documentation and controller/selection files only; all product and regression inputs are identical.

PR #1135 merged the statement-result correction as `e2f7eac1e68e648922b03a7cde4b7c9a791862b1`; its remote and local branches were removed.

## Executable source binding

Execution previously discarded the source schema returned by SQL's `bind_source_plan_schema_for_execution`, then immediately called the read-only source binder again on the same execution-owned source. The next bounded correction passes the first schema to query-block validation. SQL still owns routine identity and row-shape derivation; Execution owns the scope and transfer. No catalog, privilege or cross-statement cache is introduced.

The first binder's contract derives its returned schema from the exact routine identities installed into that source. No source, parameter, CTE or outer-scope mutation occurs before the redundant second binding. Passing this existing result therefore preserves the schema, selected routines and original error order. Pseudo-column and outer-row overlays still follow binding; expression/type/set-context and privilege checks remain in place. Runtime-defined extension sources, schemaless projections and final physical reference validation retain their existing paths. This changes metadata work only, without changing SQL row carriers or evaluating, reordering or removing user expressions.

- [x] Confirm SQL/Execution ownership, features and dependency direction.
- [x] Remove the duplicate read-only source-binding pass and retain the original execution schema.
- [x] Complete local Execution and affected Engine consumer verification, strict Clippy and ownership checks: 2,619 tests pass, including every Execution case and selected Engine table-function, LATERAL, subquery, prepared, CTE, row-lock, privilege and exact TPC-H cases. Strict Execution all-target Clippy, formatting and ownership/harness checks pass.
- [ ] Review actual automatic Linux/macOS work, memory and output results, then merge and remove the branch.

These corrections do not close #1117: repeated validation context construction, nested privilege coverage and outer-scope composition remain. The fixed TPC-H fixture, original diagnostics and controlled timing criteria remain required for the complete issue.

## Original performance task records

| Issue | Preserved work |
| --- | --- |
| #1063 | PR #1133 implements admitted regex reuse; Linux execution passes. An earlier-source controlled comparison passed all three fixed cases; final-source timing and macOS execution remain pending. |
| #348 | Remove SQLite's published dependency flag without hiding downstream Rust type/native-link compatibility; the reviewed fork delivery choice is pending. |
| #714 | PR #1134 implements bounded incremental encoding; query-result production is still materialized and needs its own correction. |
| #1108 | PR #1137 retains builtin numeric cast descriptors with deterministic 10,000-row evidence. Remaining operators, nonnumeric casts and controlled historical comparison stay open. |
| #1117 | This correction removes the duplicate statement-result pass; block binding, privilege lineage, outer scopes and controlled comparison remain open. |
| #653 | Diagnose retained instruction-throughput variation; historical inconclusive measurements remain unchanged. |
