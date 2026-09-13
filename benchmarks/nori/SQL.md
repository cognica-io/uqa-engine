# Nori public SQL and session measurements

The `uqa` facade benchmark measures the application path through existing analysis, storage, planning, execution, scoring, and Engine session interfaces. Its provider dependencies are development dependencies; it adds no runtime dependency or Engine algorithm. These measurements complement the [analysis](README.md), [Memory indexing](INDEXING.md), [physical provider indexing](PERSISTENT.md), and [operator phrase](PHRASES.md) benchmarks.

## Protocol and output verification

Four transaction workloads measure inserting 256 documents into an empty table, committing 16 documents after 256 or 2,048 existing documents, and rolling back 16 inserts after 256 existing documents. Timing includes `BEGIN`, bound SQL inserts in batches of at most 64 rows, and `COMMIT` or `ROLLBACK`. Fixture construction, closed-file copying, validation, initial open, and final database drop occur outside measurement. Each of seven timed samples, one warmup, and one allocation sample verifies every original document and six complete quoted-phrase queries against a separately constructed Memory database. SQLite and redb then close all Engine handles, reopen the actual database files, and repeat that complete validation. Reports retain the actual live and reopened snapshots.

Phrase queries use the same six fixed corpora and three decompound modes as the operator benchmark, with 2,048 repeated documents and six unrepeated source controls. Every query quotes the complete original corpus source. `sql_query` measures parameter binding, SQL analysis/planning, query analysis, physical provider reads, calibrated scoring, implicit transaction completion, and materialized results. `session_and_query` additionally measures independent persistent-session creation, `work_mem` setup, and session drop. Memory provides no independent persistent session. SQL `_score` is calibrated and is not interchangeable with the operator benchmark's raw BM25 scores. Complete document IDs and scores must agree across providers and sessions, with only `1e-12` floating roundoff tolerance.

Native measurements cover Memory, SQLite, and redb: 102 workloads. Emscripten covers Memory and SQLite: 62 workloads. Every query uses `work_mem = '256MB'`; seeds are explicitly analyzed before timing. Native databases retain their ordinary statistics worker, while Emscripten has no worker threads. Reports record this distinction and the actual SQLite version, journal mode, synchronous setting, and page size. Emscripten provider measurements use its virtual filesystem; [real browser evidence](BROWSER.md) separately verifies IndexedDB restoration and host page memory.

Allocation counters measure current-thread Rust allocator requests. Query results and mutated Engine state remain live during the allocation sample. Retained counts are signed net changes because operations can free pre-existing allocations. These values exclude stack, pre-existing fixture/dictionary allocations, SQLite C allocations, and the host heap. End-to-end provider timings include query processing and warm provider caches; they do not isolate device latency, cold I/O, or IOPS.

## Reproduction and review

```sh
python3 scripts/run-nori-sql-benchmark.py --measure-only --output target/benchmark-runs/nori-sql-native.json
python3 scripts/run-nori-sql-benchmark.py --target wasm --measure-only --output target/benchmark-runs/nori-sql-wasm.json
```

Candidate collection always validates complete provider/session coverage, original rows, analyzer identities, scores, repeated reopen evidence, counters, and sampling. It records `allocation_and_rows_passed: false` until reviewed limits are available. Repeated native and WASM CI collection supplies calibration candidates; candidate collection alone is not an allocation or timing regression pass. Source, fixture, Cargo lock, executable, compiler flags, CPU, and toolchain identities accompany each report. The runner rejects changes to measured source files during execution and output paths that overwrite reviewed inputs. Its gate tests reject missing providers or sessions, incomplete live/reopened results, altered query inputs, missing documents, changed scores, one-unit allocation regressions, and incomparable timing environments.

## Catalog fixture experiments

Fresh SQL tables have random durable table, column, and storage identities. Their JSON representation can have different lengths even when the table definition and source documents agree. Two clean native repeats preserve all complete SQL outputs but differ in transaction and some redb read allocations. An observation of eight ordinary SQL-created catalogs confirms equal observed non-identity metadata, with column metadata ranging from 638 to 647 bytes and statistics-maintenance metadata from 155 to 163 bytes. Allocation limits remain under review while these inputs and provider layouts are isolated.

The facade benchmark can capture empty SQLite and redb databases created through the same public SQL statement as its ordinary fixtures. It closes all original handles and checks columns, primary-key and not-null errors, rollback, and empty rows through separate reopened copies. Captured files retain their original identities and have recorded byte lengths and hashes. Capture requires a new directory and rejects output paths that would replace input files or the captured databases.

```sh
python3 scripts/run-nori-sql-benchmark.py --capture-empty-seeds target/benchmark-runs/nori-sql-empty-seeds --output target/benchmark-runs/nori-sql-empty-seed-capture.json
python3 scripts/run-nori-sql-benchmark.py --transaction-probe sqlite --output target/benchmark-runs/nori-sql-sqlite-fresh-catalog.json
python3 scripts/run-nori-sql-benchmark.py --transaction-probe sqlite --empty-seeds target/benchmark-runs/nori-sql-empty-seeds --output target/benchmark-runs/nori-sql-sqlite-fixed-catalog.json
```

Use `redb` for the other native provider. Each transaction experiment reuses all four existing commit/rollback workloads and their nine complete live/reopened validations, including original rows and all six quoted queries against Memory. A supplied empty fixture is loaded once and reused without modifying its bytes. Experiment reports record the selected fixture hash and remain explicitly marked as experiments with no complete regression pass; the normal SQL gate rejects their partial workload inventory. Repeated controlled measurements determine whether fixing the catalog inputs accounts for the observed allocation changes.
