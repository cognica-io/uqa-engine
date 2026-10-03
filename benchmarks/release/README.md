# Release benchmark records

Every `v*` release runs [Release Benchmarks](../../.github/workflows/release-benchmarks.yml) through the existing [Release workflow](../../.github/workflows/release.yml). It checks out the exact release tag, builds three existing Criterion executables once with the locked dependency graph, and measures the five suites in [manifest.json](manifest.json) sequentially. It does not run on ordinary pushes or pull requests. Package builds and publication have no dependency on benchmark completion; a separate job attaches reports after both the measurements and GitHub release creation finish.

The workflow follows the automatic measurement and retained-history pattern of [cognica-io/uqa's benchmark workflow](https://github.com/cognica-io/uqa/blob/b029f648b579b090bb9a5c3f07b03539b4e2f8ba/.github/workflows/benchmarks.yml). This repository stores reports as release assets and CI artifacts. Generated measurements are not committed to a branch, and shared-runner timing does not gate release publication.

## Workload inventory

| Suite | Cases | Measured boundary |
| --- | ---: | --- |
| `query_matrix` | 31 | In-memory SQL reads, joins, sets, CTEs, windows, text/vector retrieval and DML through `Engine::sql` |
| `sql_sqlite` | 11 | Native SQLite SQL reads, indexed and unindexed access, aggregation, joins, writes and session creation, including encrypted session creation |
| `sql_sqlite_kv` | 10 | The same persistent SQL workload through SQLite Key/Value |
| `sql_redb` | 10 | The same persistent SQL workload through redb |
| `retrieval_workloads` | 8 | GIN, IVF and hybrid index construction/search through native SQLite, plus in-memory graph indexing/search |

The persistent SQL suites share the same 10,000-row fixture and 50-row join table. Their untimed setup verifies row counts and sums, savepoint and transaction rollback, a new session's committed visibility and close/reopen persistence. Only native SQLite has the additional encrypted-session case. SQLite Key/Value and redb use their public persistent providers with the same default session settings. No provider is substituted with an in-memory store.

The benchmark sources define workload data and measured boundaries: [query matrix](../../crates/uqa-engine/benches/query_matrix.rs), [persistent SQL](../../crates/uqa-engine/benches/sql_sqlite_e2e.rs) and [retrieval](../../crates/uqa-engine/benches/retrieval_workloads.rs). Fixture construction and validation run outside timing. Persistent batch inserts grow the table during measurement; mutation cleanup boundaries follow the existing benchmark definitions. This is a representative release inventory, not the complete workspace benchmark or correctness suite.

## Build and measurement configuration

The workflow uses Ubuntu 24.04, Rust 1.90.0, the workspace's optimized `bench` profile with thin LTO and one codegen unit, disabled incremental compilation, and no debug information. It selects the Engine's default Cargo features. The report records compiler details, the runner image, architecture, CPU model/count, exact tag and commit, Cargo and benchmark manifest hashes, lockfile hash and executable hashes. A clean checkout establishes source provenance only; it does not establish a controlled measurement environment.

Criterion defaults supplied by the runner are a one-second warmup, three-second measurement, 30 samples and 10,000 bootstrap resamples. Existing benchmark groups retain their explicit overrides: the query matrix uses a half-second warmup and two-second measurement with 30 samples, persistent SQL uses 10–30 samples, and retrieval index construction uses 10 samples. Each result records its actual sample count, mean, median, standard errors and confidence intervals in nanoseconds. The manifest requires every named case; missing, duplicate, unexpected, nonfinite or incomplete results fail the measurement job. A failed suite does not prevent the remaining providers from being measured. There is no automatic measurement retry.

## Saved reports

Each workflow attempt produces `release-benchmarks-<tag>-<run-id>-<attempt>.json` and `.md`. The JSON contains compact estimates and provenance; Markdown contains the per-case table and execution status. Reports are attached to the corresponding GitHub release after their tag, commit, repository and run identities are checked. Attempt-specific asset names preserve earlier attempts, including failed measurements. Repeating the attachment job reuses the same report bytes and does not edit package registry status in the release notes.

Compact reports remain available as GitHub release assets and 90-day Actions artifacts. Criterion samples and build/measurement logs are separate 30-day diagnostic artifacts. Runner output is kept under `RUNNER_TEMP`, outside the Rust build cache, so a restored cache cannot supply old measurements. Partial execution is recorded as incomplete or failed, never as a complete benchmark run; setup failures without a report remain visible as failed jobs.

Measurements on shared GitHub-hosted runners are informational. Neither host control nor an independent noise bound is established, so this workflow does not claim cross-release speedups, compare against a performance acceptance threshold or block package publication for timing changes. Statistical confidence intervals describe one run's samples and do not remove host-to-host variation. Performance acceptance still requires the controlled-host evidence described in [verification](../../docs/manual/internals/09-verification.md#benchmarks).

For a new case or provider, update the owning benchmark and this inventory together. Report-verifier tests use synthetic Criterion data and never depend on saved machine reports. Changes to the workflow apply to subsequent release tags; publishing an existing tag does not execute code added after that tag.
