# Nori persistent-index measurements

The SQLite and redb benchmark entrypoints live in `uqa-storage-sqlite` and `uqa-storage-redb`. They exercise each provider's public inverted-index and transaction APIs with the pinned Nori revision. Shared measurement code lives in `benchmarks/nori/persistent.rs`; it contains fixture construction, timers, allocation instrumentation, and output verification. No Engine algorithm or runtime dependency is added. Both benchmark targets require the dependency feature `uqa-analysis/nori` and leave their provider's runtime features unchanged.

```sh
python3 scripts/run-nori-persistent-benchmark.py --provider sqlite --output target/benchmark-runs/nori-sqlite-native.json
python3 scripts/run-nori-persistent-benchmark.py --provider redb --output target/benchmark-runs/nori-redb-native.json
python3 scripts/run-nori-persistent-benchmark.py --provider sqlite --target wasm --output target/benchmark-runs/nori-sqlite-wasm.json
python3 scripts/run-nori-persistent-benchmark.py --provider sqlite --output target/benchmark-runs/nori-sqlite-repeat.json --baseline target/benchmark-runs/nori-sqlite-native.json
```

The corpus and document identities match the [Memory-index benchmark](INDEXING.md). Four workloads commit a 256-document batch into an empty index, commit 16 additional documents after 256 or 2,048 existing documents, and roll back 16 appended documents after 2,048 existing documents. A closed seed database is copied into a fresh temporary directory for every sample. Seven timed operations follow one warmup; a separate operation records allocations. Input construction, transaction begin, index mutation, and commit or rollback are measured. Seed creation/copying, connection initialization, close, graph checking, and reopen are excluded. Run measurements sequentially after other builds finish.

Every one of the nine executions per workload is checked against an independently built Memory index, both through the live provider and after dropping every database handle and reopening the file. The digest includes every document's versioned source/revision metadata, sorted canonical term keys, and complete encoded score/occurrence clusters. Document count, field length, and posting count are verified separately. The rollback workload must retain the seed graph and document count. The gate rejects missing workloads, incomplete sampling, changed graphs/counts, and omitted reopen checks.

Native measurements use files on the host temporary filesystem. The SQLite report reads its actual library version, journal mode, synchronous setting, and page size from a connection. redb uses its default immediate commit durability. SQLite WASM measurements use Emscripten's virtual filesystem in Node, so their results cover provider serialization and transaction behavior without host filesystem durability or browser storage synchronization. This matrix measures native SQLite/redb and WASM SQLite; it does not claim a WASM redb result.

Allocation counters measure current-thread Rust allocator requests during the operation. They exclude the existing index, dictionary, C allocations inside SQLite/SQLCipher, OS page cache, stack, and JavaScript heap. Net allocation can include released seed-owned buffers. File observations are the sum of closed file lengths, including remaining sidecars, rather than allocated disk blocks or a peak disk-space measurement. SQLite and redb have different transaction and durability costs; their elapsed times are reported separately.

Reports include CPU, platform, compiler versions and flags, C compiler overrides, executable hashes, runtime-source hashes, and both entrypoint/shared benchmark-source identities. Home directory prefixes in flag text become `${HOME}` for publication, while the SHA-256 of the complete original flag dictionary preserves exact comparison identity. A same-environment timing comparison rejects changed original flags, durability/filesystem scope, and benchmark code. Candidate collection uses `--measure-only` and records a false gate status until checked against reviewed limits. The instrumented benchmark is excluded from Cargo archives.

These measurements cover physical index transactions. SQL parsing, row storage, catalog assignment, Engine/session snapshots, phrase scoring, browser host memory, and release binding acceptance remain separate work in the [implementation plan](../../docs/plans/0006-nori-analyzer.md).

## Recorded baselines

Seven reports in `persistent-evidence/` were collected on an Apple M1 Ultra with Rust 1.90.0; WASM used Node 22.12.0 and Emscripten 6.0.3. Native SQLite reports version 3.50.4 and WASM SQLite reports version 3.51.3, both with WAL, synchronous value 1, and 4,096-byte pages. The reports identify the working tree based on `f7e05ebf` and include every benchmark/runtime source hash; they do not claim collection from a clean commit. `persistent-limits.json` pins every report hash and permits no allocation increase beyond the maximum observed value for its provider and pointer width. SQLite repeated every allocation counter exactly in both targets. redb repeated every counter except cumulative allocated bytes, which vary by three bytes in commit workloads.

An additional redb allocation reference retains that observed variation from an earlier measurement of the identical executable, CPU, toolchain, runtime sources, benchmark sources, and complete graph outputs. Its original compiler-flag record omitted C options, so it is explicitly excluded from timing comparisons and cannot pass as a complete benchmark report. Its measured allocation maxima contribute to the resource ceilings without padding. Published SQLite flag text was normalized from the original captured values; their full dictionary hashes preserve exact identities.

The maximum bidirectional repeat timing ratio was 1.05414 for SQLite and 1.09856 for redb. Optional timing ceilings are 1.16 and 1.21 respectively, multiplying each observed ratio by a 1.10 policy margin and rounding upward to two decimals. CI checks allocations and complete graphs on its own host without comparing Linux timing against a macOS baseline.

| Provider / target | Workload | Baseline median | Additional Rust heap peak |
| --- | --- | ---: | ---: |
| sqlite / native | `commit_batch_256/0` | 280.88 ms | 13,142,035 bytes |
| sqlite / native | `commit_batch_16/256` | 35.25 ms | 1,963,072 bytes |
| sqlite / native | `commit_batch_16/2048` | 146.98 ms | 6,958,940 bytes |
| sqlite / native | `rollback_batch_16/2048` | 120.31 ms | 6,958,940 bytes |
| redb / native | `commit_batch_256/0` | 273.25 ms | 13,159,918 bytes |
| redb / native | `commit_batch_16/256` | 36.81 ms | 5,659,869 bytes |
| redb / native | `commit_batch_16/2048` | 90.16 ms | 39,746,417 bytes |
| redb / native | `rollback_batch_16/2048` | 66.01 ms | 39,746,417 bytes |
| sqlite / wasm | `commit_batch_256/0` | 772.88 ms | 12,846,787 bytes |
| sqlite / wasm | `commit_batch_16/256` | 96.07 ms | 1,626,894 bytes |
| sqlite / wasm | `commit_batch_16/2048` | 372.94 ms | 6,932,556 bytes |
| sqlite / wasm | `rollback_batch_16/2048` | 369.62 ms | 6,932,556 bytes |
