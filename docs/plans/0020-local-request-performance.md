# Local UPDATE, DDL and request performance

Track the reported UPDATE, DDL and short-request regressions separately from the paused extension work. The implementation baseline is main `bef6f8068e7a311f040fdb46b2af187de9c441d2`; the report compares released Engine 0.3.8, 0.4.8 and 0.5.3. Each correction must preserve PostgreSQL 18 results, errors, transaction boundaries, durability and the existing memory allowances. Keep each delivery small and retain unresolved tasks in this ledger and its successor PR.

## Work and acceptance

| Area | Required investigation | State |
| --- | --- | --- |
| Multirow UPDATE | Separate row count, payload width, generated columns, indexes, JSON joins and row locks. Count private-record work, retained reads, index edits and durable arbitration; preserve exact affected rows and rollback. | Generated dependency defect #858 corrected in #859; remaining row/index/private-record attribution stays open |
| DDL | Separate one statement from a batch and compare fixed catalogs with zero and sixty unrelated tables. Attribute catalog loading, validation, publication and session creation independently. Include populated generated-column backfill. | Repeated catalog restoration reproduced as #860; local correction and bounded-work checks in progress |
| Short requests | Compare a reused Engine session, a fresh Engine session and the HTTP request boundary. Attribute admission, session construction, SQL execution, completion and response work independently. | #856 merged as `a70f100eb`, closing #857; HTTP attribution and remaining session costs stay open |
| FK deletion | Retain the already corrected indexed cascade behavior as a regression control; do not reopen the whole child-table scan as an assumed cause. | Fixed before 0.5.3 by #537; current four-provider bounded-read control passes |
| Verification | Add deterministic work/resource checks in the owning crate's existing test target, verify correctness and register relevant automatic performance regressions. Inspect actual Linux and macOS results for the delivered source before any merge. | #856 Linux/macOS complete; #859 combined-source CI pending; later corrections require their own results |

## Report provenance and workload shape

The supplied `repro.py` has SHA-256 `50f02a80cd4550b40fcfd651766b605a7cb340150e77064889445b06e6bd552d`. The two latest raw logs have SHA-256 `89987f9095860cd95c6d2c1adf0215440cb955154acbdc580977e0b212f53d61` and `96f855700061de1607cc03e587265f562d3e209da34d6be0bb3034417cb11ed0`. Raw reports remain outside version control. Their latency ratios are reported symptoms, not controlled performance acceptance; the report explicitly states that the host had concurrent load and no independently established noise bound.

The script executes scenarios sequentially against one growing schema. Its A4 UPDATE runs after E has added twenty STORED generated columns and twenty indexes to `items`; it is free of explicit JSON and locks, but not free of generated-column or index work. Each D repetition adds three tables, and D1 follows those additions. F runs after all DDL, generated columns and cascade fixtures. Preserve this application-like cumulative scenario and add isolated fixed-schema variants to distinguish causes; do not discard the original workload or attribute its full ratio to a single layer without evidence.

The short-request investigation must separate public `Engine::new_session()` from `sql_batch_diagnosed()` and from HTTP transport. Session sharing must preserve independent transaction, cancellation, variable, prepared-statement and temporary-namespace state; pooling or stale catalog reuse is not an acceptable substitute for correcting unnecessary work.

## Ownership

SQL owns expression binding and generated-column semantics; Execution owns mutation, index-maintenance and DDL algorithms. Storage owns private records, durable arbitration and provider implementations. Engine supplies session, transaction, retained-catalog and resource adapters. Inspect the owning manifests, enabled features, dependency policy and existing tests before changing these boundaries. No dependency or feature change is planned.

## Checkpoint

The supplied script and report archive have been inspected without starting their uncontrolled timing loops or touching an existing database. A separate main-based worktree preserves the paused extension changes and PR #855. Isolated synthetic databases completed plain and twenty-generated-column UPDATEs of exactly 100 rows, retained 100 final rows with an `updated_at` sum of 200, and completed a seven-statement DDL batch with sixty filler tables on the official 0.5.3 Python wheel and the current main CLI. The wheel SHA-256 is `030c82634d09b6754f74ba12e6faeb6b423ec81f380d668c25d4f4840ba01b3b`; the main CLI uses a debug build without CJK features. These executions establish correctness and diagnostic paths, not comparative latency or performance acceptance. The released wheel lacks useful Rust symbols; only main's diagnostic samples establish named call paths.

## Released versus current source

Check each reported symptom against the release and current main before treating it as an outstanding bug. Do not infer that a merged optimization resolved the complete application workload without its corresponding regression evidence.

| Path | Existing correction or current evidence | Classification |
| --- | --- | --- |
| Referencing child lookup | `9dd30e63f` (#537) uses child indexes and adds four-provider bounded-read checks; it precedes 0.5.3 and agrees with the report's G results. | Already corrected; retain the control |
| Own data-commit cache adoption | `e2bfd8318` (#580) reuses completed commit revisions; this also precedes 0.5.3. | Existing optimization; not evidence that A/A4 or F are resolved |
| Fresh-session physical bindings | `open/session_seed.rs`, `open/data_sync.rs` and `open/cache_refresh.rs` are unchanged between 0.5.3 and main `bef6f8068`. A shared session constructs document, inverted and vector stores, then replaces them immediately through full store rebinding, which also invalidates the copied row count. | Confirmed as #857; isolated correction in #856 |
| Generated expression evaluation | Main retains the analyzed assignment source type, a correctness change after 0.5.3; generated expressions still lower and bind per row. | Dependency selection corrected in #859; preserve analyzed assignment types and investigate remaining per-row work |
| DDL refresh | Current-main deterministic counts show a single CREATE restoring the complete table catalog and registries twice; a seven-statement batch repeats table restoration eight times and registry restoration eleven times. | Confirmed as #860; local correction in progress |
| Notification backend identity | A fresh public session reserves a durable identity. Both synthetic UPDATE/DDL executions allocated one identity for the entire reused session, excluding per-row identity allocation in those cases. | Request-boundary candidate only; no attribution of the reported 10 ms |

## Fresh-session correction

The #857 regression failed on all three persistent providers before the change: two tables opened four document stores. Shared session construction now retains its first document and inverted stores, installs the catalog's field analyzers, and opens each vector field once with the selected index kind after the table owner is available. Existing store replacement still invalidates row counts and value indexes. No transaction state, notification identity, mutable physical handle or independent session resource allowance is shared as a shortcut.

All three new provider cases pass, including exact store-open counts, retained row counts, independent table handles, keyword/HNSW retrieval, later commit visibility and rollback. Nine existing cases also pass: immutable catalog sharing, private and temporary catalog isolation, first-read catalog reuse, the four-provider indexed-FK control, and retained DiskANN reads. Formatting, dependency ownership and the deterministic performance inventory pass. Automatic [run 38498225580](https://github.com/cognica-io/uqa-engine/actions/runs/38498225580) passes all 467 checks / 639 cases on Linux x86_64 and macOS arm64 with zero failures or skips, plus independent-process claim correctness. Both artifacts record merge revision `788ef7e2b`, whose tree exactly matches product commit `376db800d`. PR #856 merged as `a70f100eb` and #857 closed. No measured HTTP or elapsed-time improvement is claimed, and the other workload tasks remain active.

## Generated UPDATE dependencies

PR #859 fixes independently reproduced issue #858. PostgreSQL 18.4 in Docker preserves unrelated STORED generated values on UPDATE and ON CONFLICT, considers the union of MERGE UPDATE target columns, recomputes conservatively when a BEFORE UPDATE trigger exists, and evaluates a moved row with its destination partition expressions after BEFORE INSERT triggers. Ten fixture observations distinguish these rules from both official 0.5.3 and the former main. Explicitly assigning an unchanged source still selects its dependent expressions; assigning DEFAULT to the generated column alone does not.

SQL owns dependency selection and MERGE target union; Execution applies selected expressions after row triggers and before routing, then applies destination INSERT expressions. Engine only adapts trigger descriptors. Four-provider public regressions cover the oracle, rollback, independent sessions and persistent reopen. The Execution owner regression proves that 100 rows with twenty unrelated generated columns perform zero generated evaluations, while assigning their source performs exactly 2,000; output correctness is checked in both cases. Two Execution owner tests, one SQL test, 186 Engine integration cases, strict Clippy, formatting and repository policies pass. This PR now includes the merged session correction; automatic Linux/macOS verification of the combined source remains pending. The broader UPDATE, DDL and HTTP investigations remain open.
