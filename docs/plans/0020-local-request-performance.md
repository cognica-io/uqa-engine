# Local UPDATE, DDL and request performance

Track the reported UPDATE, DDL and short-request regressions separately from the paused extension work. The implementation baseline is main `bef6f8068e7a311f040fdb46b2af187de9c441d2`; the report compares released Engine 0.3.8, 0.4.8 and 0.5.3. Each correction must preserve PostgreSQL 18 results, errors, transaction boundaries, durability and the existing memory allowances. Keep each delivery small and retain unresolved tasks in this ledger and its successor PR.

## Work and acceptance

| Area | Required investigation | State |
| --- | --- | --- |
| Multirow UPDATE | Separate row count, payload width, generated columns, indexes, JSON joins and row locks. Count private-record work, retained reads, index edits and durable arbitration; preserve exact affected rows and rollback. | Source investigation |
| DDL | Separate one statement from a batch and compare fixed catalogs with zero and sixty unrelated tables. Attribute catalog loading, validation, publication and session creation independently. Include populated generated-column backfill. | Source investigation |
| Short requests | Compare a reused Engine session, a fresh Engine session and the HTTP request boundary. Attribute admission, session construction, SQL execution, completion and response work independently. | Source investigation |
| FK deletion | Retain the already corrected indexed cascade behavior as a regression control; do not reopen the whole child-table scan as an assumed cause. | Control required |
| Verification | Add deterministic work/resource checks in the owning crate's existing test target, verify correctness and register relevant automatic performance regressions. Inspect actual Linux and macOS results for the delivered source before any merge. | Pending |

## Report provenance and workload shape

The supplied `repro.py` has SHA-256 `50f02a80cd4550b40fcfd651766b605a7cb340150e77064889445b06e6bd552d`. The two latest raw logs have SHA-256 `89987f9095860cd95c6d2c1adf0215440cb955154acbdc580977e0b212f53d61` and `96f855700061de1607cc03e587265f562d3e209da34d6be0bb3034417cb11ed0`. Raw reports remain outside version control. Their latency ratios are reported symptoms, not controlled performance acceptance; the report explicitly states that the host had concurrent load and no independently established noise bound.

The script executes scenarios sequentially against one growing schema. Its A4 UPDATE runs after E has added twenty STORED generated columns and twenty indexes to `items`; it is free of explicit JSON and locks, but not free of generated-column or index work. Each D repetition adds three tables, and D1 follows those additions. F runs after all DDL, generated columns and cascade fixtures. Preserve this application-like cumulative scenario and add isolated fixed-schema variants to distinguish causes; do not discard the original workload or attribute its full ratio to a single layer without evidence.

The short-request investigation must separate public `Engine::new_session()` from `sql_batch_diagnosed()` and from HTTP transport. Session sharing must preserve independent transaction, cancellation, variable, prepared-statement and temporary-namespace state; pooling or stale catalog reuse is not an acceptable substitute for correcting unnecessary work.

## Ownership

SQL owns expression binding and generated-column semantics; Execution owns mutation, index-maintenance and DDL algorithms. Storage owns private records, durable arbitration and provider implementations. Engine supplies session, transaction, retained-catalog and resource adapters. Inspect the owning manifests, enabled features, dependency policy and existing tests before changing these boundaries. No dependency or feature change is planned.

## Checkpoint

The supplied script and report archive have been inspected without starting their uncontrolled timing loops or touching an existing database. A separate main-based worktree preserves the paused extension changes and PR #855. No root cause or measured speedup is claimed yet.
