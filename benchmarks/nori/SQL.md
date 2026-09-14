# Nori public SQL and session measurements

The `uqa` facade benchmark measures the application path through existing analysis, storage, planning, execution, scoring, and Engine session interfaces. Its provider dependencies are development dependencies; it adds no runtime dependency or Engine algorithm. These measurements complement the [analysis](README.md), [Memory indexing](INDEXING.md), [physical provider indexing](PERSISTENT.md), and [operator phrase](PHRASES.md) benchmarks.

## Protocol and output verification

Four transaction workloads measure inserting 256 documents into an empty table, committing 16 documents after 256 or 2,048 existing documents, and rolling back 16 inserts after 256 existing documents. Timing includes `BEGIN`, bound SQL inserts in batches of at most 64 rows, and `COMMIT` or `ROLLBACK`. Fixture construction, closed-file copying, validation, initial open, and final database drop occur outside measurement. Each of seven timed samples, one warmup, and one allocation sample verifies every original document and six complete quoted-phrase queries against a separately constructed Memory database. SQLite and redb then close all Engine handles, reopen the actual database files, and repeat that complete validation. Reports retain the actual live and reopened snapshots.

Persistent fixtures begin with the pinned, empty SQL-created catalogs in [`catalogs`](../../crates/uqa/benches/nori_sql/catalogs/manifest.json). The SQLite input is 446,464 bytes and the redb input is 552,960 bytes. Their original table, column, and storage identities remain fixed while ordinary SQL creates the analyzers and GIN index, populates every source document, and analyzes each fixture. Schema-version-2 reports record the actual embedded input hashes, and the runner rejects missing or changed inputs before measurement. Input files and their capture manifest participate in the benchmark-source identity; reviewed limits must pin those same inputs. Production catalog identity generation is unchanged.

Phrase queries use the same six fixed corpora and three decompound modes as the operator benchmark, with 2,048 repeated documents and six unrepeated source controls. Every query quotes the complete original corpus source. `sql_query` measures parameter binding, SQL analysis/planning, query analysis, physical provider reads, calibrated scoring, implicit transaction completion, and materialized results. `session_and_query` additionally measures independent persistent-session creation, `work_mem` setup, and session drop. Memory provides no independent persistent session. SQL `_score` is calibrated and is not interchangeable with the operator benchmark's raw BM25 scores. Complete document IDs and scores must agree across providers and sessions, with only `1e-12` floating roundoff tolerance.

Native measurements cover Memory, SQLite, and redb: 102 workloads. Emscripten covers Memory and SQLite: 62 workloads. Every query uses `work_mem = '256MB'`; seeds are explicitly analyzed before timing. Native databases retain their ordinary statistics worker, while Emscripten has no worker threads. Reports record this distinction and the actual SQLite version, journal mode, synchronous setting, and page size. Emscripten provider measurements use its virtual filesystem; [real browser evidence](BROWSER.md) separately verifies IndexedDB restoration and host page memory.

Allocation counters measure current-thread Rust allocator requests. Query results and mutated Engine state remain live during the allocation sample. Retained bytes and counts are signed net changes because operations can free pre-existing allocations. Fixture and dictionary construction, stack, SQLite C allocations, and the host heap are outside measurement, but freeing a pre-existing Rust allocation during the operation subtracts from its retained counters. End-to-end provider timings include query processing and warm provider caches; they do not isolate device latency, cold I/O, or IOPS.

## Reproduction and review

```sh
python3 scripts/run-nori-sql-benchmark.py --output target/benchmark-runs/nori-sql-native-baseline.json
python3 scripts/run-nori-sql-benchmark.py --baseline target/benchmark-runs/nori-sql-native-baseline.json --output target/benchmark-runs/nori-sql-native-repeat.json
python3 scripts/run-nori-sql-benchmark.py --target wasm --output target/benchmark-runs/nori-sql-wasm.json
```

Normal collection verifies every provider/session result and the [reviewed allocation ceilings](sql-limits.json). Supplying `--baseline` also checks comparable environments and each timing ratio. CI executes two fresh complete reports and checks timing in both directions; its reverse-comparison file rechecks the original baseline and is not a third measurement. `--measure-only` remains available for candidate collection and records `allocation_and_rows_passed: false`. Candidate collection alone is not a regression pass. Source, fixture, Cargo lock, executable, compiler flags, CPU, and toolchain identities accompany each report. The runner rejects changes to measured source files during execution and output paths that overwrite reviewed inputs. Its gate tests reject missing providers or sessions, incomplete live/reopened results, altered query inputs, missing documents, changed scores, one-unit allocation regressions, and incomparable timing environments.

## Reviewed limits and timing interpretation

`sql-limits.json` retains complete expected results, exact target/workload allocation ceilings, and historical source references. Allocation validation remains independent of optional timing comparison. Generated full reports and intermediate diagnostics belong in ignored output directories and CI artifacts.

The effective ceiling for each signed retained counter is `max(0, recorded_ceiling)`. A negative calibration requires no positive net growth; it does not require future implementations to free the same amount of pre-existing storage. For example, rollback can restore an unchanged shared snapshot instead of replacing its original buffers with compacted copies. Both the raw observation and recorded calibration remain signed and unchanged. Positive retained ceilings and all total/peak ceilings apply exactly, and an increase of one byte or allocation above the effective ceiling fails. These counters bound net growth during the measured operation, not the absolute size of the retained database or allocation leaks hidden by unrelated frees.

The explicit timing comparison retains its 1.44 ceiling and rejects incomparable reports. A passing or failing pair on a shared or otherwise uncontrolled host does not establish performance acceptance or a runtime regression. Apply the [controlled-host timing policy](README.md#report-storage-and-timing-interpretation); repeated noisy trials must not delay code review and deterministic verification.

## Capturing catalog fixtures

Fresh SQL tables have random durable table, column, and storage identities whose serialized lengths can differ. The benchmark captures empty SQLite and redb databases through its ordinary public SQL statement, closes all handles, and verifies columns, constraints, rollback, and empty rows through reopened copies. Captures preserve their original identities and pin byte lengths and hashes. Capture requires a new directory and rejects overwriting inputs or captured databases.

```sh
python3 scripts/run-nori-sql-benchmark.py --capture-empty-seeds target/benchmark-runs/nori-sql-empty-seeds --output target/benchmark-runs/nori-sql-empty-seed-capture.json
python3 scripts/run-nori-sql-benchmark.py --transaction-probe sqlite --empty-seeds target/benchmark-runs/nori-sql-empty-seeds --output target/benchmark-runs/nori-sql-sqlite-fixed-catalog.json
```

Use `redb` for the other native provider. A transaction probe reuses the four commit/rollback workloads and all nine complete live/reopened validations. It reads the supplied captured bytes without modifying them. Its report is explicitly partial, and the normal SQL gate rejects its incomplete workload inventory. Schema-version-2 and historical foreground-scheduling reports remain readable; comparison still requires matching scheduling policies and their recorded identities.
