# Verification and Evidence

UQA Engine uses layered verification because algebra, SQL compatibility, storage atomicity, retrieval exactness, graph semantics, bindings, and performance have different oracles. A green unit test in one layer does not replace end-to-end evidence in another.

## Verification layers

```mermaid
flowchart TD
    A[Unit and law tests] --> B[Crate integration tests]
    B --> C[Engine domain harnesses]
    C --> D[Persistent reopen and failure tests]
    D --> E[Compatibility and parity fixtures]
    E --> F[Benchmarks with correctness gates]
    F --> G[Release and binding artifacts]
```

## Test taxonomy

| Evidence | Examples |
| --- | --- |
| Algebraic laws | `uqa-core` document and posting algebra, graph algebra, RPQ algebra |
| Property and differential tests | WAND exactness against exhaustive scoring, join correctness, parser fuzz-style tests |
| SQL domain tests | DDL, DML, CTE, joins, windows, JSON, temporal, routines, retrieval, graph SQL |
| Storage tests | Catalog round trips, document stores, B-tree indexes, clustered postings, SQLite and redb parity |
| Atomicity tests | Catalog, document, schema, graph, index, callback, transaction, and savepoint failure paths |
| Reopen tests | SQLite DML, vector indexes, graph path indexes, analyzers, scoring parameters, routines |
| Compatibility fixtures | PostgreSQL AGE shapes, TPC-H-derived PostgreSQL 18 results, SQL golden files |
| Binding tests | CLI integration and parity, Python, Node.js, and WASM package checks in their build workflows |

## Standard workspace gates

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

The workspace declares `unsafe_code = "deny"` and `unused_must_use = "deny"`. Clippy enables `all` and `pedantic` with an explicit small allowlist in the root manifest.

## Focused tests

Integration domains can be selected by test harness and module path:

```sh
cargo test -p uqa-engine --test integration engine_queries::sql_joins::
cargo test -p uqa-engine --test integration sql_tpch::
cargo test -p uqa-scoring --test integration wand_exactness::
cargo test -p uqa-graph --test integration rpq::
cargo test -p uqa-sql --test integration parser_fuzz::
```

Choose the narrow command during iteration, then run every affected crate and cross-crate engine path. A storage or transaction change requires memory and persistent coverage plus close and reopen.

## Architecture and repository policies

```sh
python3 scripts/check-workspace-dependencies.py
python3 scripts/check-integration-test-harnesses.py
python3 scripts/check-benchmark-coverage.py
python3 scripts/check-engine-capabilities.py
bash scripts/check-rust-file-headers.sh
bash scripts/check-rust-file-lines.sh
bash scripts/check-public-repository-hygiene.sh
```

The dependency checker enforces crate layering. The harness checker prevents uncontrolled integration-test process growth. Benchmark coverage ensures workload entry points and semantic evidence remain represented. The capability checker loads `scripts/engine-capability-policy.json`, rejects `Engine` access in declared leaf modules, rejects undeclared or stale adapter exceptions, requires the capability module's data types to match its explicit inventory, rejects service traits, and prevents `Engine` data fields, aliases, function parameters or returns, dereferences, catch-all service names, and recovery methods. Header, line, and hygiene scripts enforce repository publication rules.

The Rust line checker loads `scripts/rust-file-line-policy.json`. Every hand-maintained file at or above 1,000 physical lines must have an exact checked-in baseline, any shrink must lower or remove that baseline in the same change, and an unlisted file cannot reach 1,000 lines. Imported `uqa-pg-query` sources and build output under `target` remain excluded. Reproduce the `cloc`, physical-line, per-crate, SQL concentration, `Engine` coupling, and root-lint baseline together with:

```sh
bash scripts/measure-rust-refactoring.sh
```

Use the consolidated `uqa-engine` integration target as the fixed compile/link runner. An empty target directory measures a clean offline build and link; rerunning the same command in that directory records the warm no-op baseline without creating another test executable:

```sh
runner_target=$(mktemp -d /private/tmp/uqa-rust-fixed-runner.XXXXXX)
env CARGO_TARGET_DIR="$runner_target" /usr/bin/time -p cargo test -p uqa-engine --test integration --no-run --locked --offline
```

The 2026-08-31 structural baseline on Rust and Cargo 1.90.0 for `aarch64-apple-darwin` measured 142.76 seconds clean and 0.30 seconds warm. Absolute time is machine-specific; the stable runner, locked dependency graph, offline mode, and empty-versus-warm target distinction make later measurements comparable on the same host.

The capability, read-path, and mutation-protocol boundaries have focused executable evidence inside the existing library targets and the crate's single integration target:

```sh
cargo test -p uqa-engine --lib engine_capabilities::tests::
cargo test -p uqa-engine --lib sql::select::schema_binding::tests::
cargo test -p uqa-engine --lib sql::select::physical_plan::tests::
cargo test -p uqa-engine --lib sql::dml::protocol::
cargo test -p uqa-engine --lib sql::dml::insert::codec::tests::
cargo test -p uqa-engine --lib sql::dml::merge::codec::tests::
cargo test -p uqa-engine --lib sql::dml::view_triggers::merge::codec::tests::
cargo test -p uqa-execution --lib scalar::traversal::tests::
cargo test -p uqa-engine --test integration engine_catalog::capability_boundaries::
```

The library filters exercise immutable catalog snapshots, relation-name resolution, a deterministic complete-query binder fixture without `Engine`, physical construction from a bound plan and explicit runtime capabilities, complete physical-scalar traversal, exactly-one transaction-frame selection, overlay cleanup, and strict round trips and malformed-input rejection for prepared mutation spill rows. The integration filter executes virtual catalog reads against session state and a persistent `CREATE SCHEMA` lifecycle covering sibling isolation, rollback, commit, and reopen; DML integration filters cover every command and action family through the same protocol, so none of this evidence is compile-only.

## Exactness oracles

WAND and Block-Max WAND output is compared with exhaustive BM25 over the same postings and parameters. Approximate vector indexes are compared with brute-force cosine top-K. Join algorithms are compared with a simpler semantic path or property-generated expected result. Graph codec round trips compare complete payload, not only support identities.

An optimization may reduce work only after its result is compared with the exact oracle across ties, duplicates, NULLs, errors, and parameter boundaries.

## Compatibility evidence

The TPC-H-derived fixture runs all 22 queries and compares exact columns, row order, NULLs, text bytes, and type-aware canonical numeric values with checked-in PostgreSQL 18.4 output:

```sh
cargo test -p uqa-engine --test integration sql_tpch::
```

The live compatibility runner validates the manifest and executes every side-effect-free probe against PostgreSQL 18.4 and the release `usql` binary. At this revision `probes.sql` contains 797 probes; result rows must match after the documented normalization, and rejected statements must have the same SQLSTATE:

```sh
cargo build --release -p uqa-cli
python3 tests/parity/pg18/run_diff.py --validate-manifest
python3 tests/parity/pg18/run_diff.py
```

Stateful compatibility suites keep one PostgreSQL schema while reopening the UQA database between cases. They cover 129 routine cases, 136 role and routine-security cases, 162 constraint cases, 49 type-and-temporal cases, 584 trigger cases, 194 rewrite-rule cases, and 61 transaction cases:

The automatic-view cases include nested computed and nonautomatic rule-backed views, scalar, `EXISTS`, and `IN` subqueries in view projections and predicates, correlated and unqualified references, local-alias collisions, statement snapshots, `OLD` and `NEW` row images, check options, `MERGE`, rewrite-rule images, lazy rule input projection, `WITH CHECK OPTION` over non-updatable sources, `ONLY` partition-view insert routing, replication-independent catalog flags, no-relation star errors, and unqualified system-column rewrite cardinality.

The trigger-backed view `MERGE` cases cover direct and nested targets, INSERT, UPDATE, DELETE, and DO NOTHING actions, action-path selection and errors, statement event order, current, OLD, and NEW row images, NULL suppression, repeated candidates, replication-mode suppression, user-rule rejection, final check options, hidden target rows, failure atomicity, and statement-start snapshots.

```sh
python3 tests/parity/pg18/run_routines_stateful.py
python3 tests/parity/pg18/run_routines_stateful.py --suite roles
python3 tests/parity/pg18/run_routines_stateful.py --suite constraints
python3 tests/parity/pg18/run_routines_stateful.py --suite type-temporal
python3 tests/parity/pg18/run_routines_stateful.py --suite triggers
python3 tests/parity/pg18/run_routines_stateful.py --suite rules
python3 tests/parity/pg18/run_routines_stateful.py --suite transactions
```

Run the live TPC-H and driver gates when query output or PostgreSQL-facing I/O changes:

```sh
python3 scripts/run-tpch-pg18.py --iterations 3
bash tests/parity/pg18/clients/run.sh
```

Graph compatibility tests exercise AGE-shaped `cypher` calls and `agtype` behavior. SQL golden fixtures cover deterministic engine output. The [parity design](../../design/parity.md) records fixture provenance and update rules.

## Benchmarks

Benchmarks are performance evidence only when they declare data, warmup, sample count, measured boundary, build profile, executable provenance, correctness gates, and limitations.

Useful focused commands include:

```sh
cargo bench -p uqa-engine --bench text_top_k --locked -- --warm-up-time 2 --measurement-time 5 --sample-size 30 --noplot
bash scripts/run-vector-search-benchmark.sh
bash scripts/run-beir-benchmark.sh
python3 scripts/run-analytical-regression.py <base-commit>
```

Benchmark results are same-machine regression evidence unless independently reproduced. Cross-engine ratios are advisory and require matching semantics and measured boundaries. See the [performance design](../../design/performance.md).

## Failure testing

Inject failure before persistence, during provider write, during callback execution, during spill iteration, during commit, and during reopen migration where the subsystem permits it. Assert the original durable and visible state remains intact and that the error reaches the caller.

A failure test must verify absence of partial rows, indexes, graph objects, models, registry entries, or epoch publication. Merely checking that an error was returned is insufficient.

## Documentation verification

Manual changes should check:

- Every relative link resolves.
- Every new document is ASCII-only.
- Paragraphs are stored on one physical line, excluding structural Markdown, tables, code, Mermaid, and LaTeX blocks.
- Mermaid fences are balanced and diagrams use valid identifiers.
- SQL and API examples match current tests or compile in a focused fixture.
- Compatibility pages list unsupported shapes discovered while documenting the implementation.

## Change-specific minimums

| Change | Required evidence |
| --- | --- |
| Carrier or optimizer law | Unit law test, counterexample cases, engine integration, exact oracle |
| SQL syntax | Compiler success and rejection tests, engine result test, compatibility update |
| Storage format | Migration, failure atomicity, reopen, provider parity, old-format fixture |
| Transactional state | Implicit, explicit, savepoint, rollback, sibling session, external commit |
| Retrieval scoring | Exact fixture, calibration or relevance evidence, physical profile |
| Approximate vector index | Exact recall, mutation, reopen, parameter boundaries |
| Graph behavior | Parser or algebra test, execution result, SQL adapter, persistence if durable |
| Binding surface | Native engine test, conversion and error test, declaration or stub update |
| Performance claim | Reproducible benchmark with semantic gate and provenance |

## Review rule

Correctness claims belong to tests and explicit invariants. Performance claims belong to reproducible evidence. Neither should be inferred from code shape or a single successful example.
