# UQA-RS

UQA-RS is an embeddable database engine that lets one application use PostgreSQL-oriented SQL, full-text search, vector search, graph queries, and ranked retrieval through a shared Rust runtime.

It is designed for applications that need more than a relational table but do not want to assemble a separate database, search server, vector store, and graph engine for every query path.

## What you can build

- Run relational queries, joins, aggregates, CTEs, windows, JSON operations, and transactions with a PostgreSQL-oriented SQL surface.
- Search text with BM25 or Bayesian BM25, retrieve vectors with KNN, and combine both signals in hybrid queries.
- Store named graphs, execute Cypher and regular path queries, and call graph traversal or centrality functions from SQL.
- Start in memory for experiments, then choose the default SQLite backend or the pure-Rust redb backend without changing the query API.
- Embed the engine in Rust or use the Python, Node.js, and browser WASM bindings included in the workspace.

> [!NOTE]
> UQA-RS is under active development at version 0.1.0. The implementation is broad and heavily tested, but public APIs and storage formats may still evolve before a stable release.

## Mathematical foundation

[A Typed Carrier Algebra for Unified Query Execution](docs/papers/A%20Typed%20Carrier%20Algebra%20for%20Unified%20Query%20Execution.pdf) states the implementation-grounded theory behind UQA-RS. It distinguishes document support, weighted relations, decorated postings, ranked views, SQL bags, join tuples, graph context, and aggregate state while showing how they compose through one typed planning and execution framework.

The manuscript consolidates and revises the published work on [unified query algebra](https://doi.org/10.31219/osf.io/f56j2_v2), its [graph-data extension](https://doi.org/10.31219/osf.io/cgfae_v1), and the [Bayesian framework for hybrid search](https://doi.org/10.5281/zenodo.20768747). For academic use, cite the software and the papers relevant to the features used; machine-readable metadata is provided in [CITATION.cff](CITATION.cff).

## Try it in a terminal

You need Rust 1.90 or newer and the native build tools required by Cargo dependencies.

Start the interactive `usql` shell:

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

`uqa-engine` is the main embedded API. The following example creates an in-memory engine, inserts data, and runs SQL through the same interface used by a persistent engine.

```rust
use uqa_engine::Engine;

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

## Choose a query path

| Goal | Starting point |
| --- | --- |
| Relational SQL | `Engine::sql` or the `usql` shell |
| Streaming larger results | `Engine::sql_cursor` or `Engine::sql_columnar` |
| Full-text retrieval | `text_match`, `fts_match`, or `bayesian_match` |
| Vector retrieval | `VECTOR(N)`, `TENSOR(N)`, `knn_match`, and explicit IVF or HNSW indexes |
| Hybrid ranking | `fuse_bayesian_evidence`, `pool_positive_evidence`, or `Engine::hybrid_search` |
| Graph queries | `Engine::run_cypher`, SQL `cypher`, `rpq`, or `graph_*` functions |
| Fluent query construction | `uqa_api::QueryBuilder` |

## Persistence and encryption

`Engine::new()` keeps data in memory, while `Engine::open(path)` and `usql --db <path>` use the default persistent SQLite backend. Persistent engines restore schemas, documents, text postings, graphs, scoring parameters, models, views, and statistics when reopened.

Applications that want a pure-Rust single-file store can compose the engine with `uqa-storage-redb`. The provider owns the database, and every `Engine::new_session()` receives independent transaction state over the same file.

```rust
use std::sync::Arc;
use uqa_engine::Engine;
use uqa_storage::PersistentStorageProvider;
use uqa_storage_redb::RedbStorage;

let provider: Arc<dyn PersistentStorageProvider> =
    Arc::new(RedbStorage::open("notes.redb")?);
let engine = Engine::from_persistent_provider(provider)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The redb path supports the catalog, documents, full-text search, graphs, durable B-tree indexes, exact brute-force vectors, physical IVF and HNSW indexes, transactions, and savepoints. It uses the same SQL DDL and query API as SQLite; the main capability difference is that redb does not provide encryption at rest. See the [Key/Value storage design](docs/design/kv-storage-backends.md) for storage and transaction details.

Security-sensitive deployments should use the SQLCipher path exposed by `Engine::open_encrypted`. Compressed encrypted containers are also available when compression is required, but they have a narrower, explicitly documented threat model and require an external trusted anchor for whole-file rollback detection.

Read the [compressed VFS security contract](docs/design/compressed-vfs-security.md) before selecting that format.

## Language bindings

| Environment | Workspace package | Notes |
| --- | --- | --- |
| Rust | [`uqa-engine`](crates/uqa-engine) | Direct embedded API and runnable examples |
| Python | [`uqa-python`](crates/uqa-python) | pyo3/maturin bindings for SQL, retrieval, graph, calibration, and extensions |
| Node.js | [`uqa-node`](crates/uqa-node) | Node-API bindings with asynchronous query and search methods |
| Browser | [`uqa-wasm`](crates/uqa-wasm) | Emscripten build with SQLite persistence mounted on IndexedDB |

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

Integration tests are consolidated into a small set of domain harnesses so a workspace test does not pay one linker and process-startup cost per source file. Individual modules remain directly selectable during development:

```sh
cargo test -p uqa-engine --test engine_queries sql_joins::
cargo test -p uqa-sql --test integration parser_fuzz::
```

## PostgreSQL 17 compatibility

The repository includes a deterministic TPC-H-derived scale-factor `0.001` fixture with all 22 default queries. The self-contained correctness gate compares exact columns, row order, NULLs, text bytes, and type-aware canonical numeric values with checked-in PostgreSQL 17.10 results:

```sh
cargo test -p uqa-engine --test sql_tpch
```

Release-mode timing uses a machine-readable runner rather than test-profile execution:

```sh
cargo build --release -p uqa-engine --example tpch_runner --locked
target/release/examples/tpch_runner --iterations 201
```

In the 2026-08-09 local arm64 development snapshot, UQA matched all 22 results and had a lower median latency than PostgreSQL 17 on 14 of 22 queries. This is a small developer-machine compatibility workload, not a compliant or audited TPC-H result. The complete fixture provenance, per-query measurements, and reproduction rules are in the [TPC-H compatibility benchmark](benchmarks/tpch/README.md); the broader benchmark methodology is in the [performance design document](docs/design/performance.md).

The same 2026-08-09 workstation pass measured release-profile search hot paths with 30 Criterion samples: persisted Block-Max WAND improved from 4.7080 ms to 3.8584 ms, trained IVF top-10 over 10,000 32-dimensional vectors improved from 333.46 us to 188.80 us, and HNSW top-10 improved from 212.63 us to 148.31 us. These are same-machine regression baselines with deterministic fixtures, not portable latency claims; commands, CPU state, full tables, correctness gates, and limitations are recorded in the [performance design document](docs/design/performance.md#search-hot-path-pass-2026-08-09).

Contributor checks, benchmark build gates, and repository conventions are documented in [CONTRIBUTING.md](CONTRIBUTING.md).

## Documentation

| Document | Use it for |
| --- | --- |
| [Runnable examples](examples/README.md) | Seeing search, vectors, graphs, storage, and extensions working as complete programs |
| [Design documentation index](docs/design/README.md) | Finding the right technical contract or architecture document |
| [System architecture](docs/design/architecture.md) | Crate boundaries, query planning, carriers, execution, storage, and extension points |
| [Vector indexes](docs/design/vector-indexes.md) | Brute-force, IVF, and HNSW behavior, parameters, persistence, and correctness contracts |
| [Engine state ownership](docs/design/engine-state-ownership.md) | Session isolation, locks, epochs, and publication rules |
| [Key/Value storage](docs/design/kv-storage-backends.md) | Swappable provider contract, redb behavior, transactions, and current capability limits |
| [Compressed VFS security](docs/design/compressed-vfs-security.md) | Encryption format, authenticated metadata, rollback limits, and deployment guidance |
| [Performance](docs/design/performance.md) | Reproducible baselines, regression gates, bottlenecks, and benchmark limitations |
| [Parity fixtures](docs/design/parity.md) | SQL, relevance, and vector-calibration compatibility fixtures |
| [Citation metadata](CITATION.cff) | Software citation and DOI metadata for the underlying research papers |
| [Licensing policy](https://github.com/cognica-io/uqa-rs/blob/main/LICENSING.md) | AGPL, FOSS, noncommercial, commercial, and contribution paths |
| [History](HISTORY.md) | Release-by-release changes |

## Project layout

The repository is a Rust workspace with small crates for the algebra, storage, scoring, graph, SQL, planning, execution, engine, CLI, APIs, and language bindings. The full dependency map and ownership rules live in the [system architecture](docs/design/architecture.md), keeping this README focused on using the project.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local gates, test conventions, crate boundaries, pull request guidelines, and the current contributor-licensing requirement.

## License

UQA-RS is open-source software licensed under AGPL-3.0-only. See [LICENSE](https://github.com/cognica-io/uqa-rs/blob/main/LICENSE).

Two optional additional permissions are available:

- the [FOSS exception](https://github.com/cognica-io/uqa-rs/blob/main/LICENSES/UQA-FOSS-EXCEPTION-1.0.txt) lets a complete qualifying open-source application retain its OSI-approved license while UQA-RS and modifications to UQA-RS remain under the AGPL; and
- the [noncommercial application exception](https://github.com/cognica-io/uqa-rs/blob/main/LICENSES/UQA-NONCOMMERCIAL-EXCEPTION-1.0.txt) lets a qualifying personal, educational, academic, or charitable application keep its independent code under terms chosen by its author while UQA-RS and modifications to UQA-RS remain under the AGPL.

Separate [commercial licensing](https://github.com/cognica-io/uqa-rs/blob/main/COMMERCIAL.md) is available for proprietary applications, closed modifications, SaaS, and OEM distribution. The complete decision guide is in the [licensing policy](https://github.com/cognica-io/uqa-rs/blob/main/LICENSING.md).
