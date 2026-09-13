# Native Nori measurements

The benchmark lives in `uqa-analysis`, the owner of dictionary decoding, tokenization, and Korean filters. It calls public APIs with the `nori` feature. `allocation-counter` 0.8.1 is a pinned development dependency with no transitive dependencies; production analysis and Engine dependencies are unchanged. The benchmark does not start a JVM.

The original synthetic corpus in `crates/uqa-analysis/benches/nori/corpus.json` covers Korean prose, a short query, Hanja, mixed scripts, unknown characters, and an ambiguous unbroken 4,352-unit input. Every corpus runs through tokenizer-only and default analyzer pipelines in `None`, `Discard`, and `Mixed` decompound modes. Thirty-six output hashes cover complete serialized tokens, morphology, graph attributes, and stream end state. The three remaining measurements cover dictionary decoding and name/content-identity cache resolution.

## Reproduce and check

```sh
python3 scripts/run-nori-benchmark.py --output target/benchmark-runs/nori-native.json
python3 scripts/run-nori-benchmark.py --target wasm --output target/benchmark-runs/nori-wasm.json
python3 scripts/run-nori-benchmark.py --output target/benchmark-runs/nori-native-repeat.json --baseline target/benchmark-runs/nori-native.json
```

WASM requires the `wasm32-unknown-emscripten` Rust target, Emscripten, Node.js, and Python 3.10 or newer for Emscripten. The runner uses the same Rust benchmark in Node's WASM runtime, with memory growth, a 2 GiB maximum linear memory, and a 5 MiB stack. Set additional WASM compiler options with `CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUSTFLAGS`; global Rust flags override required target linker options and are rejected. Native execution supports the repository's Python 3.9 tooling baseline.

Run measurements sequentially on an otherwise idle machine. The `bench` profile inherits release optimization, thin LTO, one codegen unit, and debug information. Reports include exact CPU, OS, Rust/Node/Emscripten versions, flags, executable sizes and hashes, runtime source and lockfile hashes, bundle and corpus hashes, and Git/worktree identity. Benchmark executable sizes include instrumentation and debug information; they are not distribution artifact sizes.

Each timed operation creates and destroys its result. Two warmups and one pilot choose a batch size; seven samples each run for at least 75 ms, and dictionary decoding uses three samples. The timer runs between batches to avoid per-call clock overhead on cache hits. The estimator is the median of each sample's elapsed time divided by its operation count; raw elapsed times and counts remain in the report. Cold decoding means a new decoded model with validation and destruction on each operation, with warmed code and encoded-input pages; it excludes process startup and disk/network transfer.

Allocation measurement is a separate pass through the current thread's Rust System allocator. It records total, maximum simultaneous, and retained requested bytes and allocation counts. It excludes allocator metadata, stack, static bundle bytes, other threads, and the JavaScript heap. Counter updates are disabled during timing, although allocator dispatch still checks the opt-out flag. These measurements do not claim process RSS or total browser memory. Dictionary retained bytes are measured before model destruction; tokenization/filter passes must retain zero bytes after destruction. The cache probe preallocates the driver's handle vector, asserts 64 references point to the same decoded model, and measures only resolution calls and reference ownership.

Name resolution verifies resolver output because aliases can change; content-identity resolution can reuse a validated cache entry directly. Both paths share the dictionary allocation, but their timings measure different contracts. The benchmark keeps them separate.

## Regression policy

`limits.json` records the bundle and corpus identities, all expected token outputs, and separate 32-bit and 64-bit allocation ceilings from repeated native/WASM measurements. Any increase in an allocation metric, leaked output allocation, incomplete sample, changed workload, changed token graph, or uncovered measurement fails the gate. Allocation ceilings are the maximum observed values without padding; changes require measured evidence and review. The existing benchmark coverage inventory requires this analysis entrypoint and its semantic probes. Pre-merge CI requires native allocation/output checks; the JavaScript binding workflow requires the corresponding WASM check and uploads both reports.

Timing comparisons require an explicit baseline with the same CPU, OS, target, toolchain, flags, benchmark source, corpus, and sampling protocol. The maximum ratio is calibrated from repeated measurements and recorded with its evidence in `limits.json`. CI checks allocations and outputs on its own host; it does not compare Linux elapsed times with a macOS baseline. `--measure-only` writes candidate evidence with a false gate status and cannot be combined with a timing baseline. It does not rewrite the reviewed ceilings or token hashes.

These measurements cover the standalone analysis owner. [Memory indexing measurements](INDEXING.md) separately exercise the storage owner. Persistent indexing and phrase-query costs, full browser process memory, persistent binding scenarios, and release package acceptance remain separate verification work.

## Recorded baseline

The four reports in `evidence/` were collected sequentially on an Apple M1 Ultra using Rust 1.90.0, with Node 22.12.0 and Emscripten 6.0.3 for WASM. They record the working tree based on `c07bec01`, exact benchmark/runtime source hashes, and executable hashes; they do not claim a clean commit at collection time. `limits.json` pins every report's complete file hash. Both native runs and both WASM runs reproduce all 39 allocation measurements exactly, and all 36 token-stream hashes agree across targets. Maximum bidirectional repeat timing ratios were 1.04735 native and 1.03374 WASM. The optional timing ceiling is 1.16, obtained by multiplying the larger measured ratio by a policy margin of 1.10 and rounding upward to two decimal places.

| Measurement | Native | WASM |
| --- | ---: | ---: |
| Encoded static bundle | 9,829,534 bytes | 9,829,534 bytes |
| Cold decode, validate, and drop | 296.60 ms | 336.96 ms |
| Dictionary maximum requested heap | 88,384,710 bytes | 86,255,402 bytes |
| Decoded dictionary retained heap | 76,856,018 bytes | 74,726,638 bytes |
| Short-query default analysis | 9.55 µs | 20.81 µs |
| Ambiguous long-input default analysis | 3.38 ms | 7.14 ms |
| Additional heap for 64 cached handles | 0 bytes | 0 bytes |

The existing dictionary limits of 128 MiB encoded data and 256 MiB decoded sections admit the measured pinned bundle. The 32 MiB encoded-resource cache allows its default two retained dictionary identities; the measured decoded payload is about 73.3 MiB per native model and 71.3 MiB per WASM model, so the encoded cache limit is not a decoded-memory or RSS limit. No default limit was lowered from one corpus's footprint. The long input uses 4,352 UTF-16 units, exceeding the rolling lattice's 1,024-unit backtrace threshold in length; its output and allocation limits remain verified independently by adversarial tests.
