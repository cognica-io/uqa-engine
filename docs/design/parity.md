# Parity Fixtures

The Rust port keeps two kinds of golden fixtures under `tests/parity/`:

* **SQL golden file** (`sql_golden_fixture.json`) — replayed by
  [`crates/uqa-engine/tests/sql_golden.rs`](../../crates/uqa-engine/tests/sql_golden.rs).
  Each case is a `(name, sql, expected: [{column: value, ...}])`
  triple. The harness applies `schema_sql` and `data_sql` once, then
  runs every case against a fresh in-memory `Engine` and compares the
  result rows column by column.
* **BEIR-style relevance fixture** (`beir_fixture.json`) — replayed by
  [`crates/uqa-engine/tests/beir_fixture.rs`](../../crates/uqa-engine/tests/beir_fixture.rs).
  Encodes the corpus, a query set, graded judgments per query, and
  the floors for `NDCG@K` and `MAP@K` the harness must clear.

Both formats are versioned; bump `version` whenever a breaking schema
change lands and update the loader to refuse older files.

## Format reference

### SQL golden file

```json
{
  "version": 2,
  "schema_sql": ["CREATE TABLE ..."],
  "data_sql":   ["INSERT INTO ... VALUES (...)"],
  "cases": [
    {
      "name": "filter_by_label",
      "sql":  "SELECT id, label FROM notes WHERE label = 'banana'",
      "expected": [
        { "id": 2, "label": "banana" }
      ]
    }
  ]
}
```

* JSON null / object expected values map to `Value::Null`.
* Numbers map to `Value::Int` when they fit `i64`, otherwise
  `Value::Float`.
* Arrays nest as `Value::List`.

### BEIR fixture (schema v2)

```json
{
  "version": 2,
  "field": "body",
  "k": 5,
  "scorers": [
    {"name": "bm25",          "min_ndcg": 0.85, "min_map": 0.75},
    {"name": "bayesian_bm25", "min_ndcg": 0.85, "min_map": 0.75}
  ],
  "corpus": [
    {"id": 1, "body": "..."}
  ],
  "queries": [
    {
      "id": "q1",
      "text": "rust async",
      "judgments": {"1": 3.0, "4": 2.0}
    }
  ]
}
```

A v2 fixture lists every scorer it wants to gate on under `scorers`.
Each entry has a `name` (`"bm25"` or `"bayesian_bm25"`) and per-scorer
floors. The harness runs the whole query set under each declared
scorer and asserts every per-query NDCG@K and the mean MAP@K clear
that scorer's floors. `judgments` keys are JSON strings (the harness
parses them back into `u64` doc ids); values are graded relevance
scores. Documents not mentioned in the judgments map default to
relevance `0.0` for `NDCG`.

When you swap in a real BEIR run, calibrate the `min_ndcg` / `min_map`
of each scorer a few points below the observed numbers — the gate
should catch regressions without false-firing on query-set noise. If a
scorer is intentionally a regression baseline (e.g. plain BM25 vs a
calibrated Bayesian BM25), keep its entry in `scorers` so the harness
documents the expected ordering between scorers as a side effect.

## Replacing the starter fixture with a real BEIR run

To swap the small synthetic fixture for a real BEIR dataset (`scifact`,
`trec-covid`, etc.), do this offline once:

1. Download and unpack the dataset (`corpus.jsonl`, `queries.jsonl`,
   `qrels/test.tsv`).
2. Project the columns the harness expects (`id`, `body`).
3. For each query, build a `judgments` object from the `qrels` file
   (graded BEIR relevance is `0`/`1`/`2`).
4. Pick `min_ndcg` / `min_map` floors from a baseline run (BM25 or
   Bayesian BM25). Aim for a couple of points below the observed
   numbers so the gate catches regressions but tolerates noise.

A `python3 tools/parity/build_beir_fixture.py beir/scifact > beir_fixture.json`
helper script lives in the upstream Python repo's contrib directory;
the same approach works in Rust by writing a small `clap` program that
reuses `serde_json::to_string_pretty`. Either way the resulting JSON
plugs straight into `crates/uqa-engine/tests/beir_fixture.rs` without
code changes.

## CI guidance

Bench binaries should build cleanly even when CI doesn't have time to
actually run them. The cheapest gate is

```sh
cargo bench --workspace --no-run
```

which compiles every `[[bench]]` target without executing it. Pair it
with the standard suite:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo doc --workspace --no-deps
cargo deny --workspace check
```

`cargo doc --workspace --no-deps` is currently warning-free; treat any
new rustdoc warning as a regression that must be fixed before merge.

The `relevance` bench under `crates/uqa-engine/benches/relevance.rs`
replays the same BEIR fixture used by the relevance gate, prints the
mean NDCG@K and MAP@K it observed, and asserts both stay at or above
the floors in the fixture. Running it locally is the cheapest way to
catch a ranking regression *before* it reaches the test suite, since
the bench output makes any drift visible numerically.

`cargo deny check` reads `deny.toml` at the repo root and gates four
dimensions: known security advisories, license allowlist drift, banned
crates / wildcards, and unknown registries or git sources. Install it
once with `cargo install cargo-deny --locked`. The first invocation
will report any newly transitively pulled-in license that is not yet
on the allowlist; either add the license to `deny.toml` after a human
review, or replace the offending dep.

## libfuzzer / cargo fuzz (nightly cron)

The repo's `fuzz/` directory is a cargo-fuzz workspace with three
targets — `sql_compile`, `cypher_parse`, and `posting_list_round_trip`.
It is excluded from the regular workspace because it requires nightly
Rust and `libfuzzer-sys`. Set it up once on a fuzz host:

```sh
rustup install nightly
cargo install cargo-fuzz --locked
```

Run a single target locally with:

```sh
cargo +nightly fuzz run sql_compile -- -max_total_time=300
```

Phase 11 Section 7.5 calls for a nightly cron that runs each target
for ~5 minutes; the in-process proptest fuzz under
`crates/<crate>/tests/*_fuzz.rs` covers the same parsers but with
much smaller corpora and runs as part of regular `cargo test`. The
two are complementary: proptest catches obvious bugs every PR, and
cargo-fuzz finds the slow corpus-driven discoveries on the cron.

Run the full bench suite locally (or on a perf-stable runner) before
landing changes that touch the inner loop:

```sh
cargo bench -p uqa-core    --bench posting_list
cargo bench -p uqa-scoring --bench bm25
cargo bench -p uqa-scoring --bench calibration
cargo bench -p uqa-storage --bench spatial
cargo bench -p uqa-engine  --bench sql_e2e
cargo bench -p uqa-engine  --bench sql_1m
cargo bench -p uqa-engine  --bench knn
cargo bench -p uqa-engine  --bench join
cargo bench -p uqa-engine  --bench relevance
cargo bench -p uqa-graph   --bench rpq
```

## Refresh policy

* Bump `version` on any breaking change.
* Keep the diff for each case localized; add a new case for new
  behavior rather than editing existing ones.
* When a parity assertion regresses, treat it as a bug in the
  port unless the change is an intentional algorithm tweak (in which
  case update the fixture and call it out in the commit).
