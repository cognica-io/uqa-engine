# Contributing to UQA Engine

Thanks for considering a contribution. This document explains the gates every change has to clear, the conventions the codebase follows, and where new code lives.

## Contributor licensing

UQA Engine is distributed under AGPL-3.0-only, optional public exceptions, and separate commercial terms. Cognica must have sufficient rights to distribute every accepted contribution through all of those paths.

External copyrightable contributions of code or documentation require a contributor agreement accepted by Cognica. The execution workflow and final agreement are not yet published, so such contributions will not be merged until that process is available. Issues, bug reports, design discussion, and non-copyrightable factual corrections remain welcome. See [CONTRIBUTOR_POLICY.md](CONTRIBUTOR_POLICY.md) for the complete policy and the public-core commitment.

The master plan in [`docs/plans/0001-uqa-engine-implementation-plan.md`](docs/plans/0001-uqa-engine-implementation-plan.md) is the source of truth for staged deliverables and explicit deferrals. Read the relevant section before starting work on a new crate or operator.

## Local gates

Every change has to clear all of:

```sh
bash scripts/check-public-repository-hygiene.sh
python3 scripts/check-integration-test-harnesses.py
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps --locked     # rustdoc warnings are errors
cargo deny --workspace check         # cargo install cargo-deny --locked
cargo bench --workspace --no-run --locked     # benches must compile, run is opt-in
```

PR pushes run only the fast repository-hygiene and formatting gate. Once review changes are complete and the final commit is pushed, run `bash scripts/run-premerge-ci.sh`; it binds the exact remote pull-request HEAD to a temporary tag, dispatches the complete Rust, JavaScript/WebAssembly, and Python wheel suites through that immutable ref, and removes the tag after GitHub accepts the runs. A red pre-merge run is a hard block, and any later push requires one new run for the new HEAD.

If a clippy lint surfaces something legitimately user-driven, prefer fixing the root cause over adding `#[allow(clippy::...)]`. The `#[allow]` form is acceptable only with a `// reason: ...` comment and only at the smallest scope that silences it.

## Conventions

### Naming

Technology and acronym names use their canonical case. SQL is SQL, not Sql. JSON is JSON, not Json. HTML is HTML, not Html. This applies to type names, struct fields, function names, and documentation prose.

### Comments

Default to writing no comments. Add one only when the _why_ is non-obvious: a hidden constraint, a subtle invariant, a workaround for a specific bug, behavior that would surprise a reader. If removing the comment wouldn't confuse a future reader, don't write it. Don't explain _what_ the code does — well-named identifiers already do that. Don't reference the current task, fix, or callers ("used by X", "added for the Y flow", "handles the case from issue #123") — those belong in the PR description and rot as the codebase evolves.

### File scope

One main type per file. Helper types, free functions, and `#[cfg(test)] mod tests` blocks are fine alongside the main type; two top-level pub structs that each warrant their own file should each get one.

### ASCII only

Source code, doc comments, and committed text use ASCII characters only. Use `->` not `->`, `==` not `==`, plain quotes not "smart" ones. Mermaid is the diagram tool of choice for design docs; do not use ASCII art.

### Markdown paragraphs

Keep each prose paragraph on one physical line. Use blank lines only between paragraphs and structural newlines for headings, lists, tables, block quotes, and fenced code; do not hard-wrap paragraph text.

### Workarounds

The codebase does not ship workarounds, stopgap patches, or backwards-compatibility shims. If you find yourself about to describe a change as short-lived, step back and fix the root cause instead. If the root cause is genuinely out of scope, file an issue and link to it from the PR.

## Tests

The project leans heavily on `proptest` to pin algebraic invariants from the master plan. New algorithmic code should land with at least one property test that proves the relevant invariant holds for any random input, not just the hand-picked unit cases.

| Concern | Where it lives |
| --- | --- |
| Per-function unit tests | `crates/<crate>/src/<file>.rs` under `#[cfg(test)] mod tests` |
| Property tests | `crates/<crate>/tests/<area>.rs` (module of the crate's consolidated harness) |
| Cross-crate integration | `crates/uqa-engine/tests/<area>.rs` |
| Robustness fuzz (proptest-driven, runs in `cargo test`) | `crates/<crate>/tests/<area>_fuzz.rs` |
| libfuzzer fuzz (nightly cron) | `fuzz/fuzz_targets/<name>.rs` |
| SQL golden replays | `tests/parity/sql_golden_fixture.json` + `crates/uqa-engine/tests/sql_golden*.rs` |
| BEIR-style relevance gates | `tests/parity/beir_fixture.json` + `crates/uqa-engine/tests/beir_fixture.rs` |

A property test that catches a real bug should land in the same PR as the fix. Reference the bug in the commit message.

Integration test source files are modules, not independent Cargo targets. Add a new source to `tests/integration.rs`; engine tests belong in the matching `tests/engine_*.rs` resource domain. Keep TPC-H separate because it loads the complete fixture. The integration harness coverage check rejects unregistered or duplicate top-level test sources. A module can still be selected directly, for example:

```sh
cargo test -p uqa-sql --test integration parser_fuzz::
cargo test -p uqa-engine --test integration engine_queries::sql_joins::
```

Do not add a new Cargo test target merely to isolate a module during development. Use the module filter above; a separate target requires a real process-level fixture or lifecycle boundary that cannot share its harness without changing semantics.

### Performance changes

Performance work must preserve an exact correctness gate and measure an optimized package-scoped executable. For the PostgreSQL 18 TPC-H-derived fixture, run:

```sh
cargo test -p uqa-engine --test integration sql_tpch::
cargo build --release -p uqa-engine --example tpch_runner --locked
target/release/examples/tpch_runner --iterations 201
```

For vector-index performance or quality changes, run the consolidated persistent-SQL exact-ground-truth suite. Its default `standard` profile uses 100,000 128-dimensional rows; use `smoke` only for implementation checks and `large` for the explicit one-million-row scale run:

```sh
bash scripts/run-vector-search-benchmark.sh
```

For hybrid relevance, run the consolidated persistent-SQL BEIR suite. Install the pinned dependency from `benchmarks/beir/requirements.txt` once; the runner then verifies and caches the SciFact archive and pinned MiniLM model, regenerates embeddings only when their identity changes, and gates all 300 test queries:

```sh
bash scripts/run-beir-benchmark.sh
```

The vector reporter requires a persistent SQLite identity, database reopen before every phase, `Engine::sql` queries, and SQL index lifecycle statements; it then gates recall, top-1 accuracy, result completeness, and shared cosine-score accuracy before accepting Criterion latency, throughput, and one-shot SQL construction measurements. The BEIR reporter independently requires pinned real data and embeddings, persistent SQLite, SQL load and index DDL, reopen, SQL-only text/vector/hybrid queries, absolute NDCG/MAP/Recall floors, and hybrid improvement over the best single signal. Update the matching manifest, methodology README, and [`docs/design/performance.md`](docs/design/performance.md) whenever either workload identity, parameters, metric definitions, estimator, or measured boundary changes.

Record the statistic, iteration count, hardware class, toolchain, fixture identity, and whether compared runs were interleaved. Do not present a debug build, a `--quick` estimate, a sum of medians, or a non-interleaved cross-engine ratio as a regression gate. Update [`benchmarks/tpch/README.md`](benchmarks/tpch/README.md) and [`docs/design/performance.md`](docs/design/performance.md) when a measured execution boundary changes.

`prop_assert_eq!`'s format string does not support captured-variable syntax (`{var:?}`) — pass values as positional arguments. `Strategy::new_tree(...).current()` from inside a `proptest!` block bypasses proptest's case generator and shrinker; use `prop_flat_map` to tie multiple strategies together at the strategy level.

## Adding a new crate

The workspace lives under `crates/`. New crates follow the existing shape:

1. `crates/<name>/Cargo.toml` with `version.workspace = true`, `edition.workspace = true`, `license.workspace = true`, `readme = "README.md"`, `exclude.workspace = true`, and `[lints] workspace = true`. Binding crates that belong on PyPI or npm also set `publish = false`. Do not declare `license-file` alongside the SPDX `license` field, and do not inherit `readme` from the workspace because that path resolves against the workspace root.
2. `crates/<name>/src/lib.rs`.
3. Add `crates/<name>` to the `members` list in the root `Cargo.toml`.
4. Add a workspace-internal `[workspace.dependencies]` entry of the form `uqa-foo = { version = "0.1.6", path = "crates/uqa-foo" }` so other crates can depend on it via `workspace = true` without a wildcard pin (cargo deny will reject wildcards).
5. Run `python3 scripts/sync-crate-legal-files.py` so a publishable crate carries `LICENSE`, `LICENSING.md`, `LICENSE-NOTICE.md`, and both exception texts. Do not give `uqa-pg-query` those AGPL copies; it keeps its MIT and libpg_query licenses.
6. Document the new crate and its dependency boundary in `docs/design/architecture.md`; keep the repository `README.md` focused on user-facing capabilities and entry points.

## Commit messages

Commits are small and topic-focused. The first line is imperative ("Add ...", "Fix ...", "Refactor ..."), under 70 characters, and followed by a blank line. The body explains the _why_ and any relevant context that would not be obvious from the diff. Reference the master-plan section number when the change implements a named algebraic invariant or staged deliverable.

If a single PR introduces several logically distinct changes (for example, an implementation change plus a separate test harness), split it into multiple commits along the natural seams. The session-bundling commits in the initial 0.1.0 push are an example of how to structure that split: implementation, parity infrastructure, docs, benches, integration tests, property tests + fuzz.

## Pull requests

Before opening a pull request, confirm that the contribution is one of the [types that do not require an agreement](CONTRIBUTOR_POLICY.md#contributions-that-do-not-require-an-agreement) or is covered by a contributor agreement accepted by Cognica.

- Branch off `main` for every change.
- Keep the PR scope focused; reviews are easier when each PR has one reason to exist.
- The PR description should mention which gates were run locally and call out anything that needs reviewer attention (intentional divergences from upstream, performance trade-offs, deferred follow-ups).
- Squash-merge is the default; if the PR has a non-trivial multi-commit history that aids review, prefer rebase-merge.

## Filing bugs

Bug reports should include a minimal reproduction. Where a property test or fuzz target catches the bug, attach the proptest regression file (`tests/<area>.proptest-regressions`) so reviewers can replay the exact failing seed.
