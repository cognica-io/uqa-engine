# Open performance corrections

The maintainer requested completion of all open performance issues on 2026-10-20. The baseline is main `6b9f8cb87cf459f10237294565e3f444f750fcfa`. Its full local workspace run passed 18,315 tests and 53 doctests; 18 existing ignored helpers, external-client cases and manual profile probes are not passing evidence. The unrelated local edit to `tests/parity/pg18/README.md` is preserved outside these delivery worktrees.

## Delivery ledger

| Issue | Remaining correction | Owner | Status |
| --- | --- | --- | --- |
| [#1063](https://github.com/cognica-io/uqa-engine/issues/1063) | Retain at most 32 compiled patterns by text, flags and collation; account retention and preserve invocation cancellation and errors | PostgreSQL regex wrapper and SQL; Execution/Engine lifetime adapters | In progress: lifetime/interface review |
| [#348](https://github.com/cognica-io/uqa-engine/issues/348) | Remove SQLite's process-wide page-cache memory-management mutex for downstream package consumers as well as workspace builds | SQLite dependency and provider | Pending dependency delivery review; upstream fix identified |
| [#714](https://github.com/cognica-io/uqa-engine/issues/714) | Produce COPY rows and encoding incrementally with bounded ownership and sink/cancellation cleanup | Execution COPY and Engine API adapter | Pending source reconciliation and large-output regression |
| [#1108](https://github.com/cognica-io/uqa-engine/issues/1108) | Retain resolved cast/operator identities; eliminate row-time textual lookup and verify exact TPC-H results | SQL and Execution | Pending; #1113 already delivered literal casts and bounded builtin caches |
| [#1117](https://github.com/cognica-io/uqa-engine/issues/1117) | Reuse scoped block binding, validation, complete privilege coverage and outer schemas | SQL, Planner and Execution | Pending; primary RowSchema maps and Arc cloning already corrected |
| [#653](https://github.com/cognica-io/uqa-engine/issues/653) | Diagnose retained instruction-throughput variation and validate a supported correction | Controlled performance tooling/host | Pending retained-evidence investigation; historical inconclusive results remain unchanged |

Each correction has a separate reviewable PR, with logical commits pushed during work. Overlapping #1108/#1117 work is divided at typed scalar, block-analysis and privilege-scope boundaries. This ledger preserves every original issue and its acceptance conditions; a partial delivery does not close an unfinished issue.

## Verification contract

Run owner tests locally, retain exact PostgreSQL 18 reference results, and use deterministic work, memory and output checks for the changed path. Before merging, inspect the actual automatic Linux and macOS performance results and their tested source. Ordinary pushes trigger CI; do not manually dispatch, retry or cancel it. Use the existing controlled EC2 host and independent calibration for timing acceptance, keep every observation and reviewed limit, and stop the host after use. No uncontrolled repeated measurement, favorable retry or later accepted run substitutes for an unresolved inconclusive result. Generated reports stay in ignored output or CI artifacts; only fixtures, limits and compact provenance belong in Git.

## Regex lifetime correction

The current SQL call compiles through a wrapper that mutably borrows that call's resource hooks until destruction. Retaining that object directly would retain borrowed cancellation and allocation state beyond its valid lifetime. Separate the compiled allocation owner from each invocation's scratch/cancellation hooks, then retain a bounded SQL cache in the session execution context. Pattern text, compile flags and input collation all participate in lookup. A hit must still check current cancellation, and temporary matching allocations must use the current invocation's allowance. Eviction and session destruction release the original compiled reservation. Nested statements and dispatched workers must enter the correct session cache and restore their caller's context.

This is an implementation correction of existing SQL operations, with no new algebraic operator or result carrier. Reusing compiled state is valid only for identical compilation inputs; the matcher and SQL result construction continue to run for every invocation. Cache state cannot affect row multiplicity, selected spans, collation, NULL/argument validation order or diagnostic precedence.
