# UQA Engine

UQA Engine is an embeddable database engine that lets one application use PostgreSQL-oriented SQL, full-text search, vector search, graph queries, and ranked retrieval through a shared Rust runtime.

It is designed for applications that need more than a relational table but do not want to assemble a separate database, search server, vector store, and graph engine for every query path.

> [!IMPORTANT]
> **Open source with broad application exceptions**
>
> UQA Engine uses AGPL-3.0-only as its base license, with FOSS and noncommercial application exceptions. Qualifying open-source applications, including commercial ones, and qualifying personal, educational, academic, or charitable applications may keep their independent code under their own licenses or chosen terms. In practice, separate commercial terms are mainly needed for proprietary commercial products or services that must keep their application or UQA Engine changes closed. UQA Engine and modifications to UQA Engine remain under the AGPL when using the public paths. See the [licensing policy](LICENSING.md) for the exact conditions.

> [!TIP]
> **Using an LLM or coding agent?**
>
> Start with [`llms.txt`](llms.txt). It maps the authoritative manual, implementation, examples, and verification workflow without requiring the agent to load the entire repository.

## What you can build

- Run relational queries, joins, aggregates, CTEs, windows, JSON operations, and transactions with a PostgreSQL-oriented SQL surface.
- Search text with BM25 or Bayesian BM25, retrieve vectors with KNN, and combine both signals in hybrid queries.
- Store named graphs, execute Cypher and regular path queries, and call graph traversal or centrality functions from SQL.
- Start in memory for experiments, then choose the default SQLite backend or the pure-Rust redb backend without changing the query API.
- Use the same SQL result and parameter shapes against a local or Cloud UQA node through authenticated Rust, Python, Node.js, and browser HTTP engines.
- Embed the engine in Rust or use the Python, Node.js, and browser WASM bindings included in the workspace.

> [!NOTE]
> UQA Engine is under active development at version 0.1.7. The implementation is broad and heavily tested, but public APIs and storage formats may still evolve before a stable release.

## Mathematical foundation

[A Typed Carrier Algebra for Unified Query Execution](docs/papers/A%20Typed%20Carrier%20Algebra%20for%20Unified%20Query%20Execution.pdf) states the implementation-grounded theory behind UQA Engine. It distinguishes document support, weighted relations, decorated postings, ranked views, SQL bags, join tuples, graph context, and aggregate state while showing how they compose through one typed planning and execution framework.

The manuscript consolidates and revises the published work on [unified query algebra](https://doi.org/10.31219/osf.io/f56j2_v2), its [graph-data extension](https://doi.org/10.31219/osf.io/cgfae_v1), and the [Bayesian framework for hybrid search](https://doi.org/10.5281/zenodo.20768747). For academic use, cite the software and the papers relevant to the features used; machine-readable metadata is provided in [CITATION.cff](CITATION.cff).

## Try it in a terminal

Install the prebuilt Python package to get both the Python binding and the `usql` command:

```sh
python -m pip install uqa
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

Python and Node.js provide matching `local` and `cloud` project constructors; browsers retain explicit URL/token and environment construction because they cannot execute the CLI or access its credential store. Every client calls `/v1/sql`, `/v1/sql/batch`, and `/v1/sql/stream` directly after construction. See the [HTTP Engine reference](docs/manual/reference/09-http-engine.md) for connection, binding examples, result, streaming, CORS, and security contracts.

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

The redb path supports the catalog, documents, full-text search, graphs, durable B-tree indexes, exact brute-force vectors, physical IVF and HNSW indexes, transactions, and savepoints. It uses the same SQL DDL and query API as SQLite; the main capability difference is that redb does not provide encryption at rest. See the [Key/Value storage design](docs/design/kv-storage-backends.md) for storage and transaction details.

Persistent full-text indexes use clustered postings rather than one physical row or key per `(term, doc_id)`: one `(table, field, term, doc_id / 65,536)` value stores delta-encoded document IDs, term frequencies, and document lengths, while positions live in a separate value. Ranking opens score-only cursors, reuses one decode buffer for at most 128 postings per block, and leaves positional payloads unread unless a positional consumer asks for them. SQLite schema v22 and the shared Key/Value backend automatically migrate the previous per-document posting format on open; each SQLite or redb rewrite is atomic, idempotent, and rolls back without changing the old data when validation fails.

Security-sensitive deployments should use the SQLCipher path exposed by `Engine::open_encrypted`. Compressed encrypted containers are also available when compression is required, but they have a narrower, explicitly documented threat model and require an external trusted anchor for whole-file rollback detection.

Read the [compressed VFS security contract](docs/design/compressed-vfs-security.md) before selecting that format.

## Language bindings

| Environment | Workspace package | Notes |
| --- | --- | --- |
| Rust facade | [`uqa`](crates/uqa) | Primary dependency re-exporting `uqa-engine` and `uqa_core::Value` |
| Rust engine | [`uqa-engine`](crates/uqa-engine) | Direct embedded implementation API and runnable examples |
| Rust HTTP | [`uqa-client`](crates/uqa-client) | Authenticated local and Cloud data-plane SQL, atomic batches, and NDJSON streaming |
| Python | [`uqa-python`](crates/uqa-python) | pyo3/maturin bindings, the installed `usql` command, and synchronous local and Cloud HTTP SQL |
| Node.js | [`uqa-node`](crates/uqa-node) | Node-API bindings with asynchronous embedded and local or Cloud HTTP SQL methods |
| Browser | [`uqa-wasm`](crates/uqa-wasm) | Emscripten embedded engine with IndexedDB persistence plus fetch-based local and Cloud HTTP SQL |

Released Node.js applications install `@cognica-io/uqa` from npm; npm selects an exact-version native optional package under `@cognica-io` for the current supported platform. Browser applications install the independent `@cognica-io/uqa-wasm` package.

Prebuilt Linux Python wheels target glibc 2.28 or newer because the bundled DuckDB runtime requires the modern C++11 ABI.

## Build and test

Build the complete workspace:

```sh
cargo build --workspace --locked
```

Run the test suite:

```sh
cargo test --workspace --locked
```

Run the optimized persistent text top-k engine benchmark; `cargo bench` builds and executes the release benchmark profile, this target uses a real SQLite file rather than the in-memory engine, and it invokes `Engine::search_profiled` directly so cursor and ranking costs remain isolated from SQL planning and row projection:

```sh
cargo bench -p uqa-engine --bench text_top_k --locked -- --warm-up-time 2 --measurement-time 5 --sample-size 30 --noplot
```

Run the optimized persistent SQL vector-search performance and exact-ground-truth quality benchmark. The default profile loads 100,000 128-dimensional vectors into a real SQLite file, reopens it for each exact, IVF, and HNSW phase, and drives every query through `Engine::sql`:

```sh
bash scripts/run-vector-search-benchmark.sh
```

Run the real-data BEIR hybrid-search benchmark after installing its pinned Python embedding dependency. The runner downloads and verifies SciFact, generates MiniLM embeddings, loads a persistent SQLite file through SQL, creates GIN and HNSW indexes through SQL, reopens it, and executes all BM25, vector, and hybrid queries through `Engine::sql`:

```sh
python3 -m pip install -r benchmarks/beir/requirements.txt
bash scripts/run-beir-benchmark.sh
```

The combined report includes exact, IVF, and HNSW SQL query latency and throughput, SQL load and index-construction throughput, recall@10, top-1 accuracy, MRR@10, exact top-k set rate, and cosine-score error. Pass `smoke` or `large` to select the 10,000-row or 1,000,000-row profile; the deterministic workload, measured boundary, metric definitions, quality floors, output files, and limitations are documented in the [vector-search benchmark](benchmarks/vector-search/README.md).

Integration tests are consolidated into a small set of domain harnesses so a workspace test does not pay one linker and process-startup cost per source file. Individual modules remain directly selectable during development:

```sh
cargo test -p uqa-engine --test integration engine_queries::sql_joins::
cargo test -p uqa-sql --test integration parser_fuzz::
```

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

Stateful routine, constraint, type-and-temporal, trigger, and rewrite-rule oracles plus the pinned psycopg, pgx, and node-postgres matrix are documented in [PG18 differential probes](https://github.com/cognica-io/uqa-engine/blob/main/tests/parity/pg18/README.md). The current milestone and open-gate ledger is the [PostgreSQL 18 compatibility plan](https://github.com/cognica-io/uqa-engine/blob/main/docs/plans/0003-postgresql-18-compatibility.md).

Release-mode timing uses a machine-readable runner rather than test-profile execution:

```sh
cargo build --release -p uqa-engine --example tpch_runner --locked
target/release/examples/tpch_runner --iterations 201
```

In the 2026-08-09 local arm64 development snapshot, UQA matched all 22 results and had a lower median latency than PostgreSQL 17 on 14 of 22 queries. This is a small developer-machine compatibility workload, not a compliant or audited TPC-H result. The complete fixture provenance, per-query measurements, and reproduction rules are in the [TPC-H compatibility benchmark](benchmarks/tpch/README.md); the broader benchmark methodology is in the [performance design document](docs/design/performance.md).

The 2026-08-11 clustered-posting pass measured release-profile persisted Block-Max WAND at 1.0142 ms and WAND at 0.9337 ms on the direct 5,000-document reopened-SQLite probe, down 73.7% and 76.5% from the preceding 3.8584 ms and 3.9801 ms baselines. The 2026-08-12 pinned SciFact run separately measured the current exact `hybrid_log_odds` contract at 0.7226 NDCG@10, 0.6820 MAP@10, 0.8322 Recall@10, and 3.29 ms per query; it passed every absolute and comparative gate. Commands, measured boundaries, validity rules, complete tables, and limitations are recorded in the [performance design document](docs/design/performance.md#clustered-posting-pass-2026-08-11).

Contributor checks, benchmark build gates, and repository conventions are documented in [CONTRIBUTING.md](CONTRIBUTING.md).

## Documentation

| Document | Use it for |
| --- | --- |
| [Reference manual and tutorials](docs/manual/README.md) | Learning the engine, supported SQL, public APIs, and internal architecture |
| [Runnable examples](examples/README.md) | Comparing the same search, vector, graph, storage, and extension scenarios across Rust, Python, Node.js, and Browser WASM |
| [Design documentation index](docs/design/README.md) | Finding the right technical contract or architecture document |
| [System architecture](docs/design/architecture.md) | Crate boundaries, query planning, carriers, execution, storage, and extension points |
| [Vector indexes](docs/design/vector-indexes.md) | Brute-force, IVF, and HNSW behavior, parameters, persistence, and correctness contracts |
| [Vector-search benchmark](benchmarks/vector-search/README.md) | Reproducing vector latency, throughput, construction cost, recall, and accuracy reports |
| [Engine state ownership](docs/design/engine-state-ownership.md) | Session isolation, locks, epochs, and publication rules |
| [Key/Value storage](docs/design/kv-storage-backends.md) | Swappable provider contract, redb behavior, transactions, and current capability limits |
| [Compressed VFS security](docs/design/compressed-vfs-security.md) | Encryption format, authenticated metadata, rollback limits, and deployment guidance |
| [Performance](docs/design/performance.md) | Reproducible baselines, regression gates, bottlenecks, and benchmark limitations |
| [Parity fixtures](docs/design/parity.md) | SQL, relevance, and vector-calibration compatibility fixtures |
| [Citation metadata](CITATION.cff) | Software citation and DOI metadata for the underlying research papers |
| [Licensing policy](https://github.com/cognica-io/uqa-engine/blob/main/LICENSING.md) | AGPL, FOSS, noncommercial, commercial, and contribution paths |
| [History](HISTORY.md) | Release-by-release changes |

## Project layout

The repository is a Rust workspace with small crates for the algebra, storage, scoring, graph, SQL, planning, execution, engine, CLI, APIs, and language bindings. The full dependency map and ownership rules live in the [system architecture](docs/design/architecture.md), keeping this README focused on using the project.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local gates, test conventions, crate boundaries, pull request guidelines, and the current contributor-licensing requirement.

## License

UQA Engine is open-source software licensed under AGPL-3.0-only. See [LICENSE](https://github.com/cognica-io/uqa-engine/blob/main/LICENSE).

Two optional additional permissions are available:

- the [FOSS exception](https://github.com/cognica-io/uqa-engine/blob/main/LICENSES/UQA-FOSS-EXCEPTION-1.0.txt) lets a complete qualifying open-source application retain its OSI-approved license while UQA Engine and modifications to UQA Engine remain under the AGPL; and
- the [noncommercial application exception](https://github.com/cognica-io/uqa-engine/blob/main/LICENSES/UQA-NONCOMMERCIAL-EXCEPTION-1.0.txt) lets a qualifying personal, educational, academic, or charitable application keep its independent code under terms chosen by its author while UQA Engine and modifications to UQA Engine remain under the AGPL.

Separate [commercial licensing](https://github.com/cognica-io/uqa-engine/blob/main/COMMERCIAL.md) is available for proprietary applications, closed modifications, SaaS, and OEM distribution. The complete decision guide is in the [licensing policy](https://github.com/cognica-io/uqa-engine/blob/main/LICENSING.md).
