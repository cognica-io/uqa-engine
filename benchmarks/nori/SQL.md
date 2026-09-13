# Nori public SQL and session measurements

The `uqa` facade benchmark measures the application path through existing analysis, storage, planning, execution, scoring, and Engine session interfaces. Its provider dependencies are development dependencies; it adds no runtime dependency or Engine algorithm. These measurements complement the [analysis](README.md), [Memory indexing](INDEXING.md), [physical provider indexing](PERSISTENT.md), and [operator phrase](PHRASES.md) benchmarks.

## Protocol and output verification

Four transaction workloads measure inserting 256 documents into an empty table, committing 16 documents after 256 or 2,048 existing documents, and rolling back 16 inserts after 256 existing documents. Timing includes `BEGIN`, bound SQL inserts in batches of at most 64 rows, and `COMMIT` or `ROLLBACK`. Fixture construction, closed-file copying, validation, initial open, and final database drop occur outside measurement. Each of seven timed samples, one warmup, and one allocation sample verifies every original document and six complete quoted-phrase queries against a separately constructed Memory database. SQLite and redb then close all Engine handles, reopen the actual database files, and repeat that complete validation. Reports retain the actual live and reopened snapshots.

Persistent fixtures begin with the pinned, empty SQL-created catalogs in [`catalogs`](../../crates/uqa/benches/nori_sql/catalogs/manifest.json). The SQLite input is 446,464 bytes and the redb input is 552,960 bytes. Their original table, column, and storage identities remain fixed while ordinary SQL creates the analyzers and GIN index, populates every source document, and analyzes each fixture. Schema-version-2 reports record the actual embedded input hashes, and the runner rejects missing or changed inputs before measurement. Input files and their capture manifest participate in the benchmark-source identity; reviewed limits must pin those same inputs. Production catalog identity generation is unchanged.

Phrase queries use the same six fixed corpora and three decompound modes as the operator benchmark, with 2,048 repeated documents and six unrepeated source controls. Every query quotes the complete original corpus source. `sql_query` measures parameter binding, SQL analysis/planning, query analysis, physical provider reads, calibrated scoring, implicit transaction completion, and materialized results. `session_and_query` additionally measures independent persistent-session creation, `work_mem` setup, and session drop. Memory provides no independent persistent session. SQL `_score` is calibrated and is not interchangeable with the operator benchmark's raw BM25 scores. Complete document IDs and scores must agree across providers and sessions, with only `1e-12` floating roundoff tolerance.

Native measurements cover Memory, SQLite, and redb: 102 workloads. Emscripten covers Memory and SQLite: 62 workloads. Every query uses `work_mem = '256MB'`; seeds are explicitly analyzed before timing. Native databases retain their ordinary statistics worker, while Emscripten has no worker threads. Reports record this distinction and the actual SQLite version, journal mode, synchronous setting, and page size. Emscripten provider measurements use its virtual filesystem; [real browser evidence](BROWSER.md) separately verifies IndexedDB restoration and host page memory.

Allocation counters measure current-thread Rust allocator requests. Query results and mutated Engine state remain live during the allocation sample. Retained counts are signed net changes because operations can free pre-existing allocations. These values exclude stack, pre-existing fixture/dictionary allocations, SQLite C allocations, and the host heap. End-to-end provider timings include query processing and warm provider caches; they do not isolate device latency, cold I/O, or IOPS.

## Reproduction and review

```sh
python3 scripts/run-nori-sql-benchmark.py --output target/benchmark-runs/nori-sql-native-baseline.json
python3 scripts/run-nori-sql-benchmark.py --baseline target/benchmark-runs/nori-sql-native-baseline.json --output target/benchmark-runs/nori-sql-native-repeat.json
python3 scripts/run-nori-sql-benchmark.py --target wasm --output target/benchmark-runs/nori-sql-wasm.json
```

Normal collection verifies every provider/session result and the [reviewed allocation ceilings](sql-limits.json). Supplying `--baseline` also checks comparable environments and each timing ratio. CI executes two fresh complete reports and checks timing in both directions; its reverse-comparison file rechecks the original baseline and is not a third measurement. `--measure-only` remains available for candidate collection and records `allocation_and_rows_passed: false`. Candidate collection alone is not a regression pass. Source, fixture, Cargo lock, executable, compiler flags, CPU, and toolchain identities accompany each report. The runner rejects changes to measured source files during execution and output paths that overwrite reviewed inputs. Its gate tests reject missing providers or sessions, incomplete live/reopened results, altered query inputs, missing documents, changed scores, one-unit allocation regressions, and incomparable timing environments.

## Reviewed limits and remaining timing acceptance

Seven complete original reports are retained byte for byte in [`sql-evidence`](sql-evidence), with hashes, source identities, and original gate statuses in `sql-limits.json`. All complete outputs agree. Each target/workload/counter ceiling is exactly its largest observed allocation value, with no padding. The SQLite statement-cache experiment below explains its two macOS allocation states. The three-byte redb commit differences match the provider's [table-root update-order experiment](persistent-evidence/redb-commit-allocation-probe.json); the macOS reports cover both values for all three commit workloads, while the original Linux pair observes both values for `commit_16/256`. Fresh gated Linux collection must verify the remaining observed ceilings before final acceptance.

| Target | Complete workloads per report | Allocation reference | Timing calibration |
| --- | ---: | --- | --- |
| macOS aarch64 | 102 | Three complete reports | Pending; the original shift and confirmation still fail the timing ceiling |
| Linux x86_64 | 102 | Two complete reports | Maximum bidirectional repeat ratio 1.2260139597171253 |
| Emscripten wasm32 | 62 | Two complete reports | Maximum bidirectional repeat ratio 1.3080659909144803 |

The timing ceiling is `ceil(1.3080659909144803 * 1.1 * 100) / 100 = 1.44`, derived from the comparable CI pairs. The native timing follow-up below explains why the original macOS timings are not calibration inputs. Their complete output/allocation evidence remains included, and regression tests require the observed macOS timing shifts to fail the same 1.44 ceiling. A fresh complete macOS pair and fresh native/WASM gated CI remain required; the isolated single-query diagnostics do not establish that acceptance.

## Catalog fixture experiments

Fresh SQL tables have random durable table, column, and storage identities. Their JSON representation can have different lengths even when the table definition and source documents agree. Two clean native repeats preserve all complete SQL outputs but differ in transaction and some redb read allocations. An observation of eight ordinary SQL-created catalogs confirms equal observed non-identity metadata, with column metadata ranging from 638 to 647 bytes and statistics-maintenance metadata from 155 to 163 bytes. Allocation limits remain under review while these inputs and provider layouts are isolated.

The facade benchmark can capture empty SQLite and redb databases created through the same public SQL statement as its ordinary fixtures. It closes all original handles and checks columns, primary-key and not-null errors, rollback, and empty rows through separate reopened copies. Captured files retain their original identities and have recorded byte lengths and hashes. Capture requires a new directory and rejects output paths that would replace input files or the captured databases.

```sh
python3 scripts/run-nori-sql-benchmark.py --capture-empty-seeds target/benchmark-runs/nori-sql-empty-seeds --output target/benchmark-runs/nori-sql-empty-seed-capture.json
python3 scripts/run-nori-sql-benchmark.py --transaction-probe sqlite --output target/benchmark-runs/nori-sql-sqlite-fresh-catalog.json
python3 scripts/run-nori-sql-benchmark.py --transaction-probe sqlite --empty-seeds target/benchmark-runs/nori-sql-empty-seeds --output target/benchmark-runs/nori-sql-sqlite-fixed-catalog.json
```

Use `redb` for the other native provider. Each transaction experiment reuses all four existing commit/rollback workloads and their nine complete live/reopened validations, including original rows and all six quoted queries against Memory. A supplied empty fixture is loaded once and reused without modifying its bytes. Experiment reports record the selected fixture hash and remain explicitly marked as experiments with no complete regression pass; the normal SQL gate rejects their partial workload inventory. Repeated controlled measurements determine whether fixing the catalog inputs accounts for the observed allocation changes.

The [controlled experiment evidence](sql-fixture-evidence/manifest.json) records two clean executions per provider at `9c213b2c`, using identical benchmark binaries and captured files. All six allocation counters agree for every one of the four transaction workloads in both SQLite and redb, and complete live/reopened results agree with the original full SQL runs. The SQLite file also opens in the actual CI WASM package: empty rows and columns, both constraints, rollback, a Nori GIN write and complete quoted phrase, and reopened source rows and scores pass under its virtual filesystem. This is input-portability evidence, without a browser durability claim. The capture manifest retains its original dirty-worktree provenance; its three recorded benchmark source hashes match the subsequently committed generator at `9c213b2c`.

These transaction controls establish the inputs for the subsequent complete SQL/session reports. Their timings are not calibration evidence. The complete native/WASM reports now supply the reviewed limits above; the macOS timing follow-up and fresh gated CI remain open.

## SQLite session allocation control

The complete pinned macOS pair has one differing SQLite session/query allocation record: `sqlite/session_and_query/none/korean_prose`. Two separate controlled executions now reproduce every counter in both original observations and every scored result. The [raw reports and trace receipt](sql-fixture-evidence/sqlite-session-cache-evidence.json) retain their hashes, exact diagnostic source identities, pinned dependency versions, and verification scope. All 284 registry dependency versions and checksums match the workspace lockfile.

Each diagnostic first measures the warmed query. After the same query warmups, it substitutes a fresh, identically configured physical connection over the same database and prepares one unrelated `SELECT 1` outside allocation counting. It then measures the existing public independent-session, work_mem, query, and session-drop operation with its complete result retained. A final warmed observation returns to the original lower counters. Both processes reproduce the same warm/fresh/warm sequence. The setup uses existing public provider and driver interfaces:

```rust
let fresh = ManagedConnection::open(managed.database_path().unwrap())?;
fresh.with_mut(|replacement| managed.with_mut(|connection| {
    std::mem::swap(connection, replacement);
    drop(connection.prepare_cached("SELECT 1")?);
    Ok(())
}))?;
```

| Current-thread Rust allocation counter | Warm cache | Fresh connection with one cached statement | Difference |
| --- | ---: | ---: | ---: |
| Total bytes | 7,710,724 | 7,711,068 | 344 |
| Peak bytes | 356,101 | 357,913 | 1,812 |
| Retained bytes | 227,998 | 229,810 | 1,812 |
| Total allocations | 47,136 | 47,140 | 4 |
| Peak allocations | 1,600 | 1,606 | 6 |
| Retained allocations | 1,035 | 1,041 | 6 |

The scoped allocation traces identify three new 88-byte prepared-statement cache nodes and an 80-byte hash-table allocation replacing a 44-byte table in `hashbrown::raw::RawTable::reserve_rehash`, reached from `rusqlite::cache::StatementCache::cache_stmt`. The warmed operation also frees three pre-existing SQL cache keys of 72, 160, and 1,280 bytes; the fresh state has no such previous keys to free. Thus the total-byte difference is `3 * 88 + 80 = 344`, and the retained-byte difference is `3 * 88 + 80 - 44 + 72 + 160 + 1280 = 1812`. The cache-table replacement adds no net allocation count, giving four additional total allocations and six additional retained allocations. These measurements apply to the pinned rusqlite 0.39.0/hashlink 0.11.1/hashbrown 0.16.1 cache path on macOS aarch64.

These cache states account for the two complete SQL report observations without changing runtime code, result ownership, or accounting. The original executions were not stack-traced; the diagnostic establishes a reproducible causal path with exactly matching counters. Instrumented diagnostic timings are excluded from calibration. The macOS SQLite timing shift and final regression-gate execution remain open.

## Foreground scheduling on macOS

Native macOS collection launches the measured executable through the public Foundation `Process.qualityOfService` interface with `userInteractive` requested. The runner compiles the small [foreground launcher](../../crates/uqa/benches/nori_sql/foreground.swift) with the installed Xcode Swift toolchain before measurement, records its source/compiler/executable identities and child PID, and preserves command arguments and exit status. An actual child probe verifies QoS class 33, PID identity, argument forwarding, normal/failing exits, and refusal to overwrite an existing receipt. Rust runtime code, crate dependencies, and the workspace's unsafe-code policy are unchanged.

Schema-version-3 runner reports include this explicit process policy; other targets record their default scheduling policy. A timing comparison requires matching policies and launcher compiler/source identities. Historical version-2 reports remain readable for their original evidence, but cannot be treated as having the new scheduling control. This is a requested process policy, not CPU affinity or a guarantee of fixed core frequency. Complete repeat measurements must still pass the unchanged 1.44 ceiling.

The [scheduling control receipt](sql-timing-followup-evidence.json) records 45 verified executions of one query on the same live SQLite database. Warm samples have no page-cache misses. Changing thread QoS to background raises the median from roughly 1.62 seconds to 8.78 seconds; restoring foreground priority returns it to roughly 1.49 seconds. The observed wall and kernel CPU costs establish scheduling as a relevant measurement condition, without proving the sole cause of earlier full-report differences. The controlled single-query reports are diagnostic evidence, not substitutes for complete native/WASM gates.

## Native timing collection

Review of the original native execution history identifies concurrent work on the same host: all 23 Rust archives were packaged during the first SQLite query collection, followed by extraction, manifest checks, and repeated payload/checksum audits. The [timing follow-up receipt](sql-timing-followup-evidence.json) preserves both original report identities and the observed activity timestamps. The source worktrees were clean, but that records source provenance rather than host isolation. These activities do not prove the cause of the full 1.892-times SQLite timing difference; no CPU profile or scheduler trace was collected during that interval. The original results and allocation evidence remain available, while their timing difference cannot establish an isolated calibration ceiling.

Complete native timing repetitions must run sequentially after builds finish, without concurrent local builds, tests, packaging, broad archive audits, or other benchmarks. Lightweight source and result review can continue while measurement runs. Fresh complete repetitions must retain the same executable, source, inputs, flags, provider configuration, and every original result check. Their observed timing agreement remains an acceptance requirement; neither successful functional checks nor the completed cache-allocation diagnosis substitutes for it.

The complete confirmation collected at `6ddf019f` verifies all 102 results and the same executable, sources, flags, catalogs, and provider settings. Its SQLite session allocation matches the previously explained warm-cache observation. A three-byte total-allocation difference also appears in `redb/commit_256/0`, with its other five counters unchanged; the receipt retains this newly observed allocation vector. Timings still differ from the earlier repeat by up to 2.074 times, so removing the recorded concurrent work has not established timing stability.

The following local execution was explicitly converted to a profiling diagnostic and stopped after two scoped profiles; it produced no complete SQL report and is excluded from calibration. The first profile has only two main-thread observations. The second has 7,141 main-thread samples during transaction verification: 2,312 leaf samples in SQLite `pread` and 3,769 across the clustered occurrence varint/document/offset decoding functions. The background statistics thread is waiting in those samples. This identifies prominent execution paths, but it does not compare matched slow and fast query executions or establish the timing-variation cause. Raw profile hashes, selected symbols, and the exclusion are preserved in the follow-up receipt.

The complete foreground pair at `4c0dabdc` retains all 102 workloads with the same clean source, executable, lock, flags, scheduling policy, and complete results. Both reports pass the existing allocation ceilings, but timing still differs by up to 2.125 times, concentrated in several SQLite query samples. The [original reports and review](sql-timing-followup-evidence.json) preserve both failing comparison directions. Foreground scheduling alone has not resolved this variability; the unchanged 1.44 timing gate remains open.
