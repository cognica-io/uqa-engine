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

## Borrowed privilege lineage

SQL privilege analysis previously copied complete outer column-lineage maps while extending lexical scope for nested scalar queries, joins and LATERAL inputs. The owning analysis now passes borrowed immutable lineage references in the same inner-to-outer order. A join builds its separate ON scope only when an ON expression exists. No privilege analysis or catalog callback is omitted; view-owner, CTE and routine subjects retain their existing paths.

The source lineages remain alive for their nested synchronous analysis, and their contents do not mutate through these references. Substituting a reference for an owned clone preserves every qualified/unqualified lookup, shadowing decision, required-column union and ordered authorization call. The unused ON-scope construction had no semantic callbacks. This changes metadata ownership only, without changing query values, multiplicity, SQL evaluation or diagnostics.

The allocation regression retains a 1024-column outer relation and 24 nested scalar blocks. Before the correction it allocates 31,935,877 bytes and fails the fixed 4MiB ceiling; the same workload passes afterward. The owning SQL tests preserve correlated, ON/USING and LATERAL requirements, the invoking role and nested 42501 rejection. Independent PostgreSQL 18.4 returns the expected four result values and denies the same nested column. All 2,218 local SQL library/integration cases pass, including the unchanged maximum-size allocation rejection test. The allocation checks use the existing SQL integration harness so their global counting allocator does not alter library boundary tests. Strict SQL library/test Clippy and formatting pass. Final platform qualification remains pending.

The fixture constructs the same 24-level logical plan directly because compiling its SQL spelling independently exposed the protobuf recursion defect [#1146](https://github.com/cognica-io/uqa-engine/issues/1146). PostgreSQL accepts that spelling. The parser defect remains separate; no fixture-depth reduction or larger process stack is used to satisfy the lineage allocation check.

## Original performance task records

| Issue | Preserved work |
| --- | --- |
| #1063 | PR #1133 at `d64c09e2b` passes 42 local SQL cases; final automatic platform checks and its exact-source controlled comparison are in progress. Earlier accepted timing remains scoped to `a7022cf0a`. |
| #348 | Managed driver publication is complete. PR #1144 passes 63 local cases and twelve legacy lifecycle scenarios; current-source Linux qualification passes and macOS remains pending. |
| #714 | PR #1134 incremental encoding, #1142 bounded SELECT/output rollback and #1145 ordinary RETURNING/CTE delivery are locally verified. Rewrite-rule action/capture ownership and final platform/public-path acceptance remain. |
| #1108 | PR #1137 retains numeric descriptors with 10,000-row work evidence and current-source Linux qualification. Nonnumeric/operator/routine reuse and controlled historical comparison remain. |
| #1117 | Statement-result reuse is merged; executable source reuse and borrowed privilege lineage are delivered by this PR. Further block/outer-schema reuse, nested privilege coverage and controlled acceptance remain. |
| #653 | Main run 39317722346 independently passes all twenty controlled comparisons and stops EC2 afterward. The historical instruction-throughput variation remains unexplained. |

## Regression archive build reuse

The macOS job for source `e3619d6ff` finished its test archive but exceeded the existing 60-minute deadline while compiling the independent-process claim fixture with a different Cargo feature graph and development profile. The fixture now selects the same inventory packages and test profile as the archive, preserving its dependency overrides and reusing their libraries. A separate small Cargo workspace reproduces the previous feature/profile mismatch and verifies that the corrected command reuses every dependency artifact. Test assertions, execution limits and process-history verification are unchanged; no workflow is manually rerun or cancelled.
