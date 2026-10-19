# Reuse ordered statement analysis

This bounded correction addresses the duplicate statement-result pass in [#1117](https://github.com/cognica-io/uqa-engine/issues/1117). SQL's input preparation already derives output names, types, collations and anonymous-record fields while checking expressions in PostgreSQL order. The ordinary and procedural statement adapters discarded that schema and analyzed the converted tree again. A local work-count regression reproduces two block analyses for `SELECT id FROM items`; retaining the first result reduces this to one without introducing a cache.

## Ownership and implementation

The existing SQL preparation owner returns its result schema together with input-reuse eligibility and prepared dependencies. Statement adapters consume these values in their original catalog scope after constant conversion, composite-input retention and window normalization complete. EXPLAIN retains its separate body scope. Query-bearing commands still validate their inputs and return command metadata; mutation RETURNING keeps its names, positional types and record descriptors. Runtime expressions are not evaluated by analysis.

The correction adds no dependency or feature and changes no value operator or result carrier. SQL continues to own binding and result descriptions; Engine and Execution receive the same result contract. Both affected crate manifests, the dependency policy, existing preparation paths and the manual ownership boundaries were reviewed before implementation.

## Tasks and verification

- [x] Reproduce repeated block analysis with a deterministic owner test before changing product code.
- [x] Transfer the already-derived result through ordinary and procedural statement analysis.
- [x] Check output names and declared types independently with PostgreSQL 18.4 in Docker.
- [ ] Finish local owner, exact TPC-H, procedural and prepared-result validation on the delivery branch.
- [ ] Review accounting, error precedence and dependency/ownership checks; update HISTORY and the PR body.
- [ ] Verify actual automatic Linux and macOS bounded-work, memory and output results for the delivered source.
- [ ] Merge this correction and remove its branch.

This change does not close #1117: execution-time block binding, nested privilege coverage and outer-scope composition remain. The fixed TPC-H fixture, original diagnostics and controlled timing criteria remain required for the complete issue.

## Original performance task records

| Issue | Preserved work |
| --- | --- |
| #1063 | PR #1133 implements admitted regex reuse; actual automatic regression checks and the fixed controlled comparison are pending. |
| #348 | Remove SQLite's published dependency flag without hiding downstream Rust type/native-link compatibility; the reviewed fork delivery choice is pending. |
| #714 | PR #1134 implements bounded incremental encoding; query-result production is still materialized and needs its own correction. |
| #1108 | Retain selected cast/operator descriptors through execution; preserve temporal settings, exact output and the original controlled historical comparison. |
| #1117 | This correction removes the duplicate statement-result pass; block binding, privilege lineage, outer scopes and controlled comparison remain open. |
| #653 | Diagnose retained instruction-throughput variation; historical inconclusive measurements remain unchanged. |
