# Nori graph phrase measurements

The `uqa-operators` benchmark calls `score_phrase_budgeted` for connected graph matching and BM25 scoring. Its `analysis_and_match` workloads also call storage's `analyze_query_graph_budgeted`, so complete phrase analysis, lossless key projection, matching, provider reads, and retained results share one 256 MiB query allowance. `match_graph` workloads borrow an already analyzed immutable query. Both use the same pinned analyzer revision and the public Memory index. No Engine algorithm or runtime dependency is added; the benchmark target requires the development dependency feature `uqa-analysis/nori`.

```sh
python3 scripts/run-nori-phrase-benchmark.py --output target/benchmark-runs/nori-phrase-native.json
python3 scripts/run-nori-phrase-benchmark.py --target wasm --output target/benchmark-runs/nori-phrase-wasm.json
python3 scripts/run-nori-phrase-benchmark.py --output target/benchmark-runs/nori-phrase-repeat.json --baseline target/benchmark-runs/nori-phrase-native.json
```

The six fixed corpora cover Korean prose, a short query, Hanja, mixed scripts, unknown strings, and long ambiguous documents. Each of the None, Discard, and Mixed decompound modes uses its own compiled default analyzer pipeline with POS stops, readings, and simple lowercase. Each index contains 2,048 repeated corpus documents, distributed by document identity modulo six, plus one original unrepeated source control for each corpus. Queries use the complete original source text once. All matching document identities are checked, including the required source control, with 342 or 343 results per query. The two analysis scopes produce identical document identities, graph sizes, analyzer identities, and scores.

There are 36 workloads. One warmup precedes seven separately timed queries and one allocation query. Every execution verifies ordered document identities and exact score bits against the warmup. The report checker additionally requires the complete fixture result set and all three distinct analyzer identities. Time includes query budget construction, matching, scoring, and retained result construction; `analysis_and_match` also includes analysis and key projection. Dictionary loading, compilation, index construction, validation, and final result destruction are excluded. Run measurements sequentially without concurrent builds.

Allocation figures are current-thread Rust allocator requests, with the final scored result and its reservation retained at the end of the allocation sample. Existing dictionary/index state and pre-analyzed query input are excluded, as are stack and host heap. These figures do not measure process RSS, browser JavaScript/WASM host memory, physical-provider query I/O, SQL parsing/planning, or Engine/session transaction cost. Those remaining measurements and binding/release acceptance remain recorded in the [implementation plan](../../docs/plans/0006-nori-analyzer.md).

The runner rejects missing workloads, incomplete samples, changed source text, missing matching documents, unexpected documents, duplicate or unordered identities, nonfinite scores, analyzer identity drift, and differences between the two analysis scopes. Reviewed output contracts compare document identities exactly and scores with a relative/absolute tolerance of 1e-12; the recorded native/WASM scores agree exactly. Every allocation counter has a reviewed 32-bit or 64-bit ceiling without padding. Timing comparisons require the same CPU, platform, target, compiler, flags including their original SHA-256 identity, benchmark source, and complete result set. CI enforces allocation and output gates on native and WASM; timing comparison is explicit. `--measure-only` collects an unaccepted candidate with a false gate status.

## Recorded evidence

Four complete reports in `phrase-evidence/` record two native and two WASM executions on an Apple M1 Ultra with Rust 1.90.0, Node 22.12.0, and Emscripten 6.0.3. They identify the working tree based on `1a75c653` and contain runtime-source, benchmark, corpus, compiler-flag, and executable hashes; collection was from a dirty working tree. All six allocation counters reproduce exactly for every workload in each target. Analyzer identities, query occurrence counts, document identities, and scores also agree across native and WASM. Maximum additional requested heap is 187,416 bytes on native and 186,484 bytes on WASM.

The maximum bidirectional repeat time ratio is 1.10958. Applying the existing 1.10 timing-policy margin and rounding upward to two decimals produces a 1.23 ceiling. The four report hashes and every observed allocation ceiling are pinned in `phrase-limits.json`. The instrumented benchmark is excluded from Cargo archives.

The table shows Mixed mode; the reports contain all three modes and all timing samples.

| Scope / corpus | Native median | WASM median | Native additional heap peak | WASM additional heap peak |
| --- | ---: | ---: | ---: | ---: |
| `match_graph/korean_prose` | 8.466 ms | 22.189 ms | 33,088 bytes | 31,020 bytes |
| `analysis_and_match/korean_prose` | 8.600 ms | 22.241 ms | 68,468 bytes | 45,396 bytes |
| `match_graph/short_query` | 0.560 ms | 2.824 ms | 14,016 bytes | 13,532 bytes |
| `analysis_and_match/short_query` | 0.624 ms | 2.744 ms | 14,490 bytes | 13,958 bytes |
| `match_graph/hanja` | 3.574 ms | 9.811 ms | 24,224 bytes | 23,452 bytes |
| `analysis_and_match/hanja` | 3.833 ms | 10.110 ms | 25,020 bytes | 24,168 bytes |
| `match_graph/mixed_script` | 6.093 ms | 16.971 ms | 28,672 bytes | 26,892 bytes |
| `analysis_and_match/mixed_script` | 6.194 ms | 17.276 ms | 30,527 bytes | 28,555 bytes |
| `match_graph/unknown` | 2.453 ms | 6.861 ms | 20,272 bytes | 19,716 bytes |
| `analysis_and_match/unknown` | 2.440 ms | 6.915 ms | 20,841 bytes | 20,229 bytes |
| `match_graph/ambiguous_long` | 107.836 ms | 153.263 ms | 186,544 bytes | 185,700 bytes |
| `analysis_and_match/ambiguous_long` | 107.899 ms | 141.778 ms | 187,416 bytes | 186,484 bytes |
