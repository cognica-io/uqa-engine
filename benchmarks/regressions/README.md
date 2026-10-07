# Deterministic performance regressions

The [inventory](manifest.json) selects existing tests in their owning crates. Pull requests and main pushes that change Rust, the inventory or its runner automatically execute it on Linux and macOS through [performance-checks.yml](../../.github/workflows/performance-checks.yml). It covers indexed foreign-key reads, routine and ordinary-statement analysis reuse, bounded sorting, text occurrence and phrase memory, WAND output, HNSW/IVF spill, DiskANN resource and I/O accounting, native graph projection, durable allocation, batched key-claim arbitration, SQLite commit-cache work, version-payload WAL page counts and read-free post-commit cache adoption. The 42 separately named temporal concurrency histories remain included.

The work and memory bounds live in the referenced Rust assertions, beside their input fixtures and output checks. For example, indexed referential lookup may enumerate no child rows and may load only matching documents; 32 generic routine calls fold one immutable expression; 32 warm managed allocations require two synchronized commits. Changing a limit requires reviewing that test's workload, correctness checks and reason for the new bound. The inventory does not maintain a second copy of numeric limits that could disagree with the executable assertion.

Each entry records the package, existing harness kind, exact test function, every expected parameter-case name and their count. The runner lists compiled tests before execution and fails if a required case disappeared, was ignored, was filtered out, moved to another crate or overlapped another entry. A same-count replacement of a provider or parameter case also fails. It also rejects unexpectedly selected tests. Nextest then runs the same selection with the existing `ci` profile: no retries, a 30-second slow-test notice and a five-minute termination limit. These timeouts detect a stuck test; they are not latency regression limits.

To validate the inventory without compiling:

```sh
python3 scripts/run-performance-checks.py --check
python3 -m unittest discover -s scripts/tests -p test_performance_checks.py
```

To run it with the repository toolchain and cargo-nextest installed:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 python3 scripts/run-performance-checks.py
```

The runner builds the selected crates' existing library/integration targets once while listing tests. Its run command reuses those build artifacts. CI retains JUnit output and compact JSON/Markdown summaries for seven days. The summary records the revision, inventory hash, source hash of every selected test, compiler/nextest versions, platform and workflow attempt. Generated files live under ignored `target/`, never in the source inventory. A failing test retains nextest's assertion diagnostic; an inventory failure names the missing or skipped case before execution.

To extend coverage, add a test to the owner's current harness that asserts both output correctness and a deterministic resource bound, then add an inventory entry with the workload and invariant. Parameterize independent provider cases when useful and update the exact expected case count. Run the inventory validator and its unit tests; CI validates the compiled selection and executes it on both platforms. Existing Nori allocation limits and release workload fixtures retain their own contracts.

This inventory is the deterministic part of [#261](https://github.com/cognica-io/uqa-engine/issues/261). Controlled-runner latency/throughput acceptance with an independently established noise bound remains open. Shared-runner elapsed times, the stall timeout and successful output assertions cannot establish that acceptance, and the runner explicitly records `timing_acceptance: false`.
