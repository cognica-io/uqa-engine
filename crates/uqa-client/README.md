# UQA Engine

UQA Engine is an embeddable database engine that lets one application use a PostgreSQL 18-compatible SQL surface across the behavior covered by its differential suite, full-text search, vector search, graph queries, and ranked retrieval through a shared Rust runtime.

It is designed for applications that need more than a relational table but do not want to assemble a separate database, search server, vector store, and graph engine for every query path.

> [!IMPORTANT]
> **Open source with broad application exceptions**
>
> UQA Engine uses AGPL-3.0-only as its base license, with FOSS and noncommercial application exceptions. Qualifying open-source applications, including commercial ones, and qualifying personal, educational, academic, or charitable applications may keep their independent code under their own licenses or chosen terms. In practice, separate commercial terms are mainly needed for proprietary commercial products or services that must keep their application or UQA Engine changes closed. UQA Engine and modifications to UQA Engine remain under the AGPL when using the public paths. See the [licensing policy](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/LICENSING.md) for the exact conditions.

> [!TIP]
> **Using an LLM or coding agent?**
>
> Start with [`llms.txt`](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/llms.txt). It maps the authoritative manual, implementation, examples, and verification workflow without requiring the agent to load the entire repository.

## What you can build

- Run relational queries, joins, aggregates, CTEs, windows, JSON operations, and transactions with PostgreSQL 18 compatibility across the verified SQL surface.
- Search text with BM25 or Bayesian BM25, retrieve vectors with KNN, and combine both signals in hybrid queries.
- Store named graphs, execute Cypher and regular path queries, and call graph traversal or centrality functions from SQL.
- Start in memory for experiments, then choose the default SQLite backend or the pure-Rust redb backend without changing the query API.
- Use the same SQL result and parameter shapes against a local or Cloud UQA node through authenticated Rust, Python, Node.js, and browser HTTP engines.
- Embed the engine in Rust or use the Python, Node.js, and browser WASM bindings included in the workspace.

## New in 0.5.2

Version 0.5.2 reduces repeated SQL analysis, SQLite commit work and vector-index rewrites while preserving transaction and query semantics. It also corrects sequence identity refresh, concurrent automatic ANALYZE, cross-process claim identity and owner admission, and retained PL/pgSQL record-field types. See the [release history](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/HISTORY.md#052---2026-10-08).

Ordinary statements and routine bodies reuse valid analysis and plans without losing current parameters or snapshot checks. SQLite batches durable allocations, avoids redundant completion writes and synchronizes native WAL sequence logs at the consuming transaction boundary. HNSW restores spilled edges in bounded groups, retains certified generations after own writes and skips rewrites when canonical vectors are unchanged. Large mutation, text-index and vector-index workloads retain bounded memory through encrypted temporary storage.

The engine provides native DiskANN vector indexes through memory, native SQLite, SQLite Key/Value and redb. Bounded graph navigation and product quantization select candidates; complete-tensor reranking preserves canonical cosine scores and the existing probability conversion. Indexes retain transaction, rollback and reopen behavior, and EXPLAIN distinguishes estimated work from actual query counters. See the [SQL configuration and score contract](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/sql/02-ddl.md#diskann-vector-indexes) and [matching Rust, Python, Node.js and browser examples](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/examples/README.md).

Independent SQL notification subscriptions retain their original database and selected role, with bounded queues and explicit cleanup. Rust, Python, Node.js and Browser WASM also provide authenticated HTTP/SSE clients with visible loss and reconnection events for compatible servers. See the [direct Rust API](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/reference/02-rust-engine-api.md#independent-owned-listeners), [language bindings](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/reference/08-bindings-and-extensions.md#notification-subscriptions) and [HTTP contract](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/reference/09-http-engine.md#rust-notification-subscriptions).

Persistent databases use SQLite record format 60, native mapping 15 or redb record format 56. Initial open atomically upgrades predecessor SQLite version records, and row-claim tables and relation registries use coordination format 2. Close every owner before taking a pre-upgrade backup, update all owners together, and follow the [0.5.2 upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/reference/10-upgrading.md#052) for temporary disk requirements, custom Rust provider interfaces and rollback to an older binary.

## Mathematical foundation

[A Typed Carrier Algebra for Unified Query Execution](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/papers/A%20Typed%20Carrier%20Algebra%20for%20Unified%20Query%20Execution.pdf) states the implementation-grounded theory behind UQA Engine. It distinguishes document support, weighted relations, decorated postings, ranked views, SQL bags, join tuples, graph context, and aggregate state while showing how they compose through one typed planning and execution framework.

The manuscript consolidates and revises the published work on [unified query algebra](https://doi.org/10.31219/osf.io/f56j2_v2), its [graph-data extension](https://doi.org/10.31219/osf.io/cgfae_v1), and the [Bayesian framework for hybrid search](https://doi.org/10.5281/zenodo.20768747). For academic use, cite the software and the papers relevant to the features used; machine-readable metadata is provided in [CITATION.cff](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/CITATION.cff).

## Try it in a terminal

Install the prebuilt Python package to get both the Python binding and the `usql` command:

```sh
python -m pip install uqa==0.5.2
usql
```

To build from this repository, you need Rust 1.90 or newer and the native build tools required by Cargo dependencies.

Start the interactive `usql` shell from the repository:

```sh
cargo run -p uqa-cli --bin usql
```

Create a table and run a text search:

```sql
CREATE TABLE notes (
    id INTEGER PRIMARY KEY,
    title TEXT,
    body TEXT
);

CREATE INDEX notes_body_gin ON notes USING gin (body);

INSERT INTO notes (id, title, body) VALUES
    (1, 'Rust async', 'Futures and the Tokio runtime'),
    (2, 'Embedded Rust', 'Drivers for constrained devices'),
    (3, 'Web application', 'Forms, routing, and templates');

SELECT id, title, _score
FROM notes
WHERE text_match(body, 'rust async')
ORDER BY _score DESC
LIMIT 5;
```

Use a file-backed database by adding `--db`:

```sh
cargo run -p uqa-cli --bin usql -- --db notes.uqa
```

Execute one command without entering the shell:

```sh
cargo run -p uqa-cli --bin usql -- -c "SELECT 1 AS ready"
```

## Embed it in Rust

Add the released package to your application:

```sh
cargo add uqa@0.5.2
```

Korean and Japanese text analysis are separate optional features that this command does not enable: `nori` adds the Korean analyzer with its embedded dictionary, and `kuromoji` the Japanese one. Enable the ones an application uses, for example `cargo add uqa@0.5.2 --features nori,kuromoji` or `features = ["nori", "kuromoji"]` on the dependency in `Cargo.toml`; `uqa-engine` takes the same features. A build without them rejects requests for those analyzers. The `usql` CLI and the Python, Node.js, and browser WASM packages enable both by default.

`uqa` is the primary Rust package on crates.io. It is a thin facade over `uqa-engine` that also re-exports the core `Value` type; applications that need the implementation package directly can depend on `uqa-engine`. Public component crates including `uqa-engine`, `uqa-client`, `uqa-api`, and `uqa-cli` are also published independently. The following example creates an in-memory engine, inserts data, and runs SQL through the same interface used by a persistent engine.

```rust
use uqa::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();

    engine.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
        &[],
    )?;
    engine.sql(
        "CREATE INDEX notes_body_gin ON notes USING gin (body)",
        &[],
    )?;
    engine.sql(
        "INSERT INTO notes (id, title, body) VALUES
         (1, 'Rust async', 'Futures and the Tokio runtime'),
         (2, 'Embedded Rust', 'Drivers for constrained devices')",
        &[],
    )?;

    let result = engine.sql(
        "SELECT id, title, _score
         FROM notes
         WHERE text_match(body, 'rust async')
         ORDER BY _score DESC",
        &[],
    )?;

    for row in result.rows {
        println!("{row:?}");
    }

    Ok(())
}
```

Run the complete example from this repository:

```sh
cargo run -p uqa-engine --example text_search
```

Additional runnable examples cover hybrid search and encrypted storage:

```sh
cargo run -p uqa-engine --example hybrid_search
cargo run -p uqa-engine --example sqlcipher_encrypted_catalog
cargo run -p uqa-engine --example compressed_encrypted_catalog
```

## Connect to a local or Cloud UQA node

`uqa-client::HttpEngine` sends SQL directly to the authenticated HTTP data plane shared by local and Cloud nodes. Native applications can resolve a project through the installed CLI once during construction, while services can continue to supply an explicit URL and token or trusted `UQA_URL` and `UQA_TOKEN` environment variables.

```rust
use uqa_client::{HttpEngine, SQLParam};
use uqa_core::Value;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let engine = HttpEngine::cloud("notes", Some("example")).await?;
let result = engine
    .sql(
        "SELECT id, title FROM notes WHERE id = $1",
        &[SQLParam::scalar(Value::Int(42))],
    )
    .await?;
assert_eq!(result.rows.len(), 1);
# Ok(())
# }
```

Python and Node.js provide matching `local` and `cloud` project constructors; browsers retain explicit URL/token and environment construction because they cannot execute the CLI or access its credential store. Every client calls `/v1/sql`, `/v1/sql/batch`, and `/v1/sql/stream` directly after construction. See the [HTTP Engine reference](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/reference/09-http-engine.md) for connection, binding examples, result, streaming, CORS, and security contracts.

## Choose a query path

| Goal | Starting point |
| --- | --- |
| Relational SQL | `Engine::sql` or the `usql` shell |
| Local or Cloud SQL over HTTP | `uqa_client::HttpEngine` |
| Streaming larger results | `Engine::sql_cursor` or `Engine::sql_columnar` |
| Full-text retrieval | `text_match`, `fts_match`, or `bayesian_match` |
| Vector retrieval | `VECTOR(N)`, `TENSOR(N)`, `knn_match`, and explicit IVF or HNSW indexes |
| Hybrid ranking | Automatic mixed-modality `AND`, exact `fuse_bayesian_evidence` or `fuse_log_odds`, `Engine::hybrid_search`, or explicit robust `pool_positive_evidence` and `Engine::robust_hybrid_search` |
| Graph queries | `Engine::run_cypher`, SQL `cypher`, `rpq`, or `graph_*` functions |
| Fluent query construction | `uqa_api::QueryBuilder` |

## Persistence and encryption

`Engine::new()` keeps data in memory, while `Engine::open(path)` and `usql --db <path>` use the default persistent SQLite backend. Persistent engines restore schemas, documents, text postings, graphs, scoring parameters, models, views, and statistics when reopened.

`uqa-storage` owns provider-independent contracts and shared data structures, while `uqa-storage-sqlite` owns SQLite connections, catalogs, indexes, transactions, graph persistence, and compressed storage. Rust callers using concrete storage types must use the [updated provider imports](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/reference/10-upgrading.md#sqlite-provider-ownership).

Applications that want a pure-Rust single-file store can compose the engine with `uqa-storage-redb`. The provider owns the database, and every `Engine::new_session()` receives independent transaction state over the same file.

```rust
use std::sync::Arc;
use uqa::Engine;
use uqa_storage::PersistentStorageProvider;
use uqa_storage_redb::RedbStorage;

let provider: Arc<dyn PersistentStorageProvider> =
    Arc::new(RedbStorage::open("notes.redb")?);
let engine = Engine::from_persistent_provider(provider)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The redb path supports the catalog, documents, full-text search, graphs, durable B-tree indexes, exact brute-force vectors, physical IVF and HNSW indexes, transactions, and savepoints. It uses the same SQL DDL and query API as SQLite; the main capability difference is that redb does not provide encryption at rest. See the [Key/Value storage design](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/kv-storage-backends.md) for storage and transaction details.

Persistent full-text indexes use clustered postings rather than one physical row or key per `(term, doc_id)`: one `(table, field, term, doc_id / 65,536)` value stores delta-encoded document IDs, term frequencies, and document lengths, while positions live in a separate value. Ranking opens score-only cursors, reuses one decode buffer for at most 128 postings per block, and leaves positional payloads unread unless a positional consumer asks for them. SQLite schema v22 and the shared Key/Value backend automatically migrate the previous per-document posting format on open; each SQLite or redb rewrite is atomic, idempotent, and rolls back without changing the old data when validation fails.

Security-sensitive deployments should use the SQLCipher path exposed by `Engine::open_encrypted`. Compressed encrypted containers are also available when compression is required, but they have a narrower, explicitly documented threat model and require an external trusted anchor for whole-file rollback detection.

Read the [compressed VFS security contract](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/compressed-vfs-security.md) before selecting that format.

## Language bindings

| Environment | Workspace package | Notes |
| --- | --- | --- |
| Rust facade | [`uqa`](https://github.com/cognica-io/uqa-engine/tree/v0.5.2/crates/uqa) | Primary dependency re-exporting `uqa-engine` and `uqa_core::Value` |
| Rust engine | [`uqa-engine`](https://github.com/cognica-io/uqa-engine/tree/v0.5.2/crates/uqa-engine) | Direct embedded implementation API and runnable examples |
| Rust HTTP | [`uqa-client`](https://github.com/cognica-io/uqa-engine/tree/v0.5.2/crates/uqa-client) | Authenticated local and Cloud data-plane SQL, atomic batches, and NDJSON streaming |
| Python | [`uqa-python`](https://github.com/cognica-io/uqa-engine/tree/v0.5.2/crates/uqa-python) | pyo3/maturin bindings, the installed `usql` command, and synchronous local and Cloud HTTP SQL |
| Node.js | [`uqa-node`](https://github.com/cognica-io/uqa-engine/tree/v0.5.2/crates/uqa-node) | Node-API bindings with asynchronous embedded and local or Cloud HTTP SQL methods |
| Browser | [`uqa-wasm`](https://github.com/cognica-io/uqa-engine/tree/v0.5.2/crates/uqa-wasm) | Emscripten embedded engine with IndexedDB persistence plus fetch-based local and Cloud HTTP SQL |

Released Node.js applications install `@cognica-io/uqa` from npm; npm selects an exact-version native optional package under `@cognica-io` for the current supported platform. Browser applications install the independent `@cognica-io/uqa-wasm` package.

Prebuilt Linux Python wheels target glibc 2.28 or newer because the bundled DuckDB runtime requires the modern C++11 ABI.

## Build and test

Install the commit hook, then build and test the workspace:

```sh
bash scripts/install-git-hooks.sh
cargo build --workspace --locked
cargo test --workspace --locked
```

Run a focused test during development:

```sh
cargo test -p uqa-engine --test integration queries::sql_joins::
```

See [CONTRIBUTING.md](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/CONTRIBUTING.md) for contributor checks, [crate ownership](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/internals/01-architecture.md) for implementation boundaries, and [verification](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/internals/09-verification.md) for compatibility tests and benchmarks.

## PostgreSQL 18 compatibility

The repository includes a deterministic TPC-H-derived scale-factor `0.001` fixture with all 22 default queries. The self-contained correctness gate compares exact columns, row order, NULLs, text bytes, and type-aware canonical numeric values with checked-in PostgreSQL 18.4 results:

```sh
cargo test -p uqa-engine --test integration sql_tpch::
```

The broader PostgreSQL 18.4 gate validates the compatibility manifest and compares every checked-in value or SQLSTATE probe with a live server. Build `usql` in release mode before running it:

```sh
cargo build --release -p uqa-cli
python3 tests/parity/pg18/run_diff.py --validate-manifest
python3 tests/parity/pg18/run_diff.py
```

Stateful routine, constraint, type-and-temporal, trigger, and rewrite-rule oracles plus the pinned psycopg, pgx, and node-postgres matrix are documented in [PG18 differential probes](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/tests/parity/pg18/README.md). The current milestone and open-gate ledger is the [PostgreSQL 18 compatibility plan](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/plans/0003-postgresql-18-compatibility.md).

Release-mode timing uses a machine-readable runner rather than test-profile execution:

```sh
cargo build --release -p uqa-engine --example tpch_runner --locked
target/release/examples/tpch_runner --iterations 201
```

In the 2026-08-09 local arm64 development snapshot, UQA matched all 22 results and had a lower median latency than PostgreSQL 17 on 14 of 22 queries. This is a small developer-machine compatibility workload, not a compliant or audited TPC-H result. The complete fixture provenance, per-query measurements, and reproduction rules are in the [TPC-H compatibility benchmark](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/benchmarks/tpch/README.md); the broader benchmark methodology is in the [performance design document](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/performance.md).

The 2026-08-11 clustered-posting pass measured release-profile persisted Block-Max WAND at 1.0142 ms and WAND at 0.9337 ms on the direct 5,000-document reopened-SQLite probe, down 73.7% and 76.5% from the preceding 3.8584 ms and 3.9801 ms baselines. The 2026-08-12 pinned SciFact run separately measured the current exact `hybrid_log_odds` contract at 0.7226 NDCG@10, 0.6820 MAP@10, 0.8322 Recall@10, and 3.29 ms per query; it passed every absolute and comparative gate. Commands, measured boundaries, validity rules, complete tables, and limitations are recorded in the [performance design document](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/performance.md#clustered-posting-pass-2026-08-11).

Contributor checks, benchmark build gates, and repository conventions are documented in [CONTRIBUTING.md](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/CONTRIBUTING.md).

## Documentation

| Document | Use it for |
| --- | --- |
| [Reference manual and tutorials](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/README.md) | Learning the engine, supported SQL, public APIs, and internal architecture |
| [Runnable examples](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/examples/README.md) | Comparing the same search, vector, graph, storage, and extension scenarios across Rust, Python, Node.js, and Browser WASM |
| [Design documentation index](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/README.md) | Finding the right technical contract or architecture document |
| [System architecture](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/architecture.md) | Crate boundaries, query planning, carriers, execution, storage, and extension points |
| [Vector indexes](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/vector-indexes.md) | Brute-force, IVF, HNSW and DiskANN behavior, parameters, persistence and correctness contracts |
| [Vector-search benchmark](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/benchmarks/vector-search/README.md) | Reproducing vector latency, throughput, construction cost, recall, and accuracy reports |
| [Engine state ownership](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/engine-state-ownership.md) | Session isolation, locks, epochs, and publication rules |
| [Concurrent storage transactions](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/concurrent-storage-transactions.md) | Overlapping logical writes, snapshots, conflicts, atomic publication, and provider ownership |
| [Key/Value storage](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/kv-storage-backends.md) | Swappable provider contract, redb behavior, transactions, and current capability limits |
| [Upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/manual/reference/10-upgrading.md) | Package updates, Rust API changes, persistent format upgrades, and backup restoration |
| [Compressed VFS security](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/compressed-vfs-security.md) | Encryption format, authenticated metadata, rollback limits, and deployment guidance |
| [Performance](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/performance.md) | Reproducible baselines, regression gates, bottlenecks, and benchmark limitations |
| [Parity fixtures](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/parity.md) | SQL, relevance, and vector-calibration compatibility fixtures |
| [Citation metadata](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/CITATION.cff) | Software citation and DOI metadata for the underlying research papers |
| [Licensing policy](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/LICENSING.md) | AGPL, FOSS, noncommercial, commercial, and contribution paths |
| [History](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/HISTORY.md) | Release-by-release changes |

## Project layout

Cypher default-label validation belongs to `uqa-graph`; Engine supplies the selected graph snapshot and retains its transaction boundary. Graph tests cover nested pattern requirements and missing-label diagnostics, while durable graph lifecycle tests remain with Engine.

RPQ syntax and its parser are shared through `uqa-core`; planner estimates no longer import the graph runtime. Existing graph syntax imports remain available, and parser tests move with their implementation.

The repository is a Rust workspace with small crates for the algebra, storage, scoring, graph, SQL, planning, execution, engine, CLI, APIs, and language bindings. The full dependency map and ownership rules live in the [system architecture](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/architecture.md), keeping this README focused on using the project.

Model training uses shared native execution for Rust and SQL callers: `uqa-execution` converts projected table rows, parses training JSON, invokes `uqa-ml`, and publishes the trained result through Engine's model transaction boundary. Pure training-input and persisted IVF-parameter tests live with their owning crates. See the [ownership design](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/docs/design/sql-crate-boundaries.md).

Creation namespace selection and index-target visibility run in SQL and native execution. Engine lends live schema, role, relation, and session guards; CTAS and index creation retain their existing authorization and collision-check order.

## Contributing

See [CONTRIBUTING.md](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/CONTRIBUTING.md) for local gates, test conventions, crate boundaries, pull request guidelines, and the current contributor-licensing requirement.

AI-assisted contributions follow [AI_POLICY.md](https://github.com/cognica-io/uqa-engine/blob/main/AI_POLICY.md), including semantic preservation, algebraic proofs for feature additions, and maintainer judgment on code and design.

## License

UQA Engine is open-source software licensed under AGPL-3.0-only. See [LICENSE](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/LICENSE).

Two optional additional permissions are available:

- the [FOSS exception](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/LICENSES/UQA-FOSS-EXCEPTION-1.0.txt) lets a complete qualifying open-source application retain its OSI-approved license while UQA Engine and modifications to UQA Engine remain under the AGPL; and
- the [noncommercial application exception](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/LICENSES/UQA-NONCOMMERCIAL-EXCEPTION-1.0.txt) lets a qualifying personal, educational, academic, or charitable application keep its independent code under terms chosen by its author while UQA Engine and modifications to UQA Engine remain under the AGPL.

Separate [commercial licensing](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/COMMERCIAL.md) is available for proprietary applications, closed modifications, SaaS, and OEM distribution. The complete decision guide is in the [licensing policy](https://github.com/cognica-io/uqa-engine/blob/v0.5.2/LICENSING.md).
