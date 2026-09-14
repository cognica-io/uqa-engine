# Native Nori measurements

The benchmark lives in `uqa-analysis`, the owner of dictionary decoding, tokenization, and Korean filters. It calls public APIs with the `nori` feature. `allocation-counter` 0.8.1 is a pinned development dependency with no transitive dependencies; production analysis and Engine dependencies are unchanged. The benchmark does not start a JVM.

The original synthetic corpus in `crates/uqa-analysis/benches/nori/corpus.json` covers Korean prose, a short query, Hanja, mixed scripts, unknown characters, and an ambiguous unbroken 4,352-unit input. Every corpus runs through tokenizer-only and default analyzer pipelines in `None`, `Discard`, and `Mixed` decompound modes. Thirty-six output hashes cover complete serialized tokens, morphology, graph attributes, and stream end state. The three remaining measurements cover dictionary decoding and name/content-identity cache resolution.

## Reproduce and check

```sh
python3 scripts/run-nori-benchmark.py --output target/benchmark-runs/nori-native.json
python3 scripts/run-nori-benchmark.py --target wasm --output target/benchmark-runs/nori-wasm.json
python3 scripts/run-nori-benchmark.py --output target/benchmark-runs/nori-native-repeat.json --baseline target/benchmark-runs/nori-native.json
```

WASM requires the `wasm32-unknown-emscripten` Rust target, Emscripten, Node.js, and Python 3.10 or newer for Emscripten. The runner uses the same Rust benchmark in Node's WASM runtime, with memory growth, a 2 GiB maximum linear memory, and a 5 MiB stack. Set additional WASM compiler options with `CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUSTFLAGS`; nonempty global Rust flags override required target linker options and are rejected. The runner removes empty `RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS` from its child environment because Cargo gives their presence precedence over target flags even when they contain no options. Native execution supports the repository's Python 3.9 tooling baseline.

Run measurements sequentially on an otherwise idle machine. The `bench` profile inherits release optimization, thin LTO, one codegen unit, and debug information. Reports include exact CPU, OS, Rust/Node/Emscripten versions, flags, executable sizes and hashes, runtime source and lockfile hashes, bundle and corpus hashes, and Git/worktree identity. Benchmark executable sizes include instrumentation and debug information; they are not distribution artifact sizes.

Each timed operation creates and destroys its result. Two warmups and one pilot choose a batch size; seven samples each run for at least 75 ms, and dictionary decoding uses three samples. The timer runs between batches to avoid per-call clock overhead on cache hits. The estimator is the median of each sample's elapsed time divided by its operation count; raw elapsed times and counts remain in the report. Cold decoding means a new decoded model with validation and destruction on each operation, with warmed code and encoded-input pages; it excludes process startup and disk/network transfer.

Allocation measurement is a separate pass through the current thread's Rust System allocator. It records total, maximum simultaneous, and retained requested bytes and allocation counts. It excludes allocator metadata, stack, static bundle bytes, other threads, and the JavaScript heap. Counter updates are disabled during timing, although allocator dispatch still checks the opt-out flag. These measurements do not claim process RSS or total browser memory. Dictionary retained bytes are measured before model destruction; tokenization/filter passes must retain zero bytes after destruction. The cache probe preallocates the driver's handle vector, asserts 64 references point to the same decoded model, and measures only resolution calls and reference ownership.

Name resolution verifies resolver output because aliases can change; content-identity resolution can reuse a validated cache entry directly. Both paths share the dictionary allocation, but their timings measure different contracts. The benchmark keeps them separate.

## Regression policy

`limits.json` records the bundle and corpus identities, all expected token outputs, and separate 32-bit and 64-bit allocation ceilings from repeated native/WASM measurements. Any increase in an allocation metric, leaked output allocation, incomplete sample, changed workload, changed token graph, or uncovered measurement fails the gate. Allocation ceilings are the maximum observed values without padding; changes require measured evidence and review. The existing benchmark coverage inventory requires this analysis entrypoint and its semantic probes. Pre-merge CI requires native allocation/output checks; the JavaScript binding workflow requires the corresponding WASM check and uploads both reports.

Timing comparisons require an explicit baseline with the same CPU, OS, target, toolchain, flags, benchmark source, corpus, and sampling protocol. The maximum ratio is calibrated from repeated measurements and recorded with its evidence in `limits.json`. CI checks allocations and outputs on its own host; it does not compare Linux elapsed times with a macOS baseline. `--measure-only` writes candidate evidence with a false gate status and cannot be combined with a timing baseline. It does not rewrite the reviewed ceilings or token hashes.

These measurements cover the standalone analysis owner. [Memory indexing measurements](INDEXING.md) separately exercise the storage owner. Persistent indexing and phrase-query costs, full browser process memory, persistent binding scenarios, and release package acceptance remain separate verification work.

## Report storage and timing interpretation

Commit workload fixtures, complete expected outputs, allocation ceilings, and compact source or artifact references. Write generated measurements and intermediate diagnostics under ignored `target/benchmark-runs/` and retain CI outputs as workflow artifacts. Historical report identities in the limits link to their original source commit; raw reports are not repository fixtures. Verifier unit tests use deterministic inputs and do not depend on past machine observations.

Timing acceptance requires a controlled benchmark host and an independently established noise bound. Matching CPU labels, a clean source tree, sequential runs, or suspending this task's builds does not establish host control. Shared CI and uncontrolled workstations provide observations, not reliable performance acceptance. Do not retry noisy measurements until one passes or widen limits to accommodate them. Keep unverified performance explicitly unverified while completing functional, ownership, dependency, and allocation checks.
