# Nori cooperative cancellation measurements

The existing `uqa-analysis` benchmark has a separate `--cancellation` mode using the public budgeted tokenizer and analyzer APIs. It covers all six fixed corpora, three decompound modes, both analysis scopes, and cancellation at the first, middle, and final poll of a complete successful operation: 108 workloads. The ordinary analysis mode keeps its existing 39 measurements and reviewed gates. Both modes record the additional source module in their benchmark identity. No runtime algorithm, dependency, Cargo feature, or test target changes.

Each workload retains an unrelated 4,096-byte reservation in a 256 MiB allowance. Every cancelled call must return `AnalysisError::Cancelled`, stop at the selected poll, and restore exactly that unrelated reservation. Two warmups, seven samples of 16 interrupted operations, and one allocation observation verify 115 cancellations. Ten subsequent complete recovery operations compare every native token and stream-end field with the successful result. The report's recovered output hash and token count must also agree with the existing analysis contract. Allocation observations must retain zero additional bytes and allocations.

## Timing scope

Every interrupted operation records entry-to-return time and callback-decision-to-return time. The latter includes workspace destruction and clock-call overhead. It begins when the cooperative callback decides to return the cancellation error; it does not measure the interval between an external signal and the next poll, scheduler latency, or a preemptive interruption. Verification and recovery occur outside those intervals. Per-sample values sum 16 observations, and the estimator is the median per-operation value across seven samples. A zero value remains a valid observation below clock resolution but cannot establish a timing-comparison ratio.

Rust allocation counters cover one interrupted operation on the current thread. Preloaded dictionary/input/reference results, the allowance handle, the unrelated reservation, and host memory are outside that scope. Each new reference count and cancellation position comes from the same fixed complete input; changing the polling schedule makes a timing baseline incomparable until reviewed again.

## Collection and acceptance

```sh
python3 scripts/run-nori-cancellation-benchmark.py --measure-only --output target/benchmark-runs/nori-cancellation-native.json
python3 scripts/run-nori-cancellation-benchmark.py --target wasm --measure-only --output target/benchmark-runs/nori-cancellation-wasm.json
```

Candidate collection validates all 108 workloads, complete input/output identities, every reported cancellation and recovery, unrelated-reservation preservation, allocation counters, and both timing estimators. Reports retain source, executable, compiler-flag, CPU, and toolchain provenance. A candidate records `allocation_and_recovery_passed: false`; successful collection is not a reviewed performance pass. The checker supports exact target-specific allocation ceilings and explicit same-environment comparisons of both timing scopes, but limits must come from actual repeated measurements. The workflow rechecks the existing ordinary analysis gate and collects two cancellation candidates for each of native and WASM. The tagged `uqa-nori-cancellation/**` trigger permits that workflow to run independently during other long measurements; the required Rust workflow also calls it.

All 238 repository tooling tests pass, together with ownership/dependency, harness, source-header, hygiene, and workflow checks. Native/WASM execution, repeated local measurements, and reviewed cancellation regression limits remain open in the [implementation plan](../../docs/plans/0006-nori-analyzer.md). No cancellation latency or allocation result is claimed before those measurements finish.
