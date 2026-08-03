# UQA-RS

UQA-RS is an embeddable database engine that lets one application use PostgreSQL-oriented SQL, full-text search, vector search, graph queries, and ranked retrieval through a shared Rust runtime.

It is designed for applications that need more than a relational table but do not want to assemble a separate database, search server, vector store, and graph engine for every query path.

## What you can build

- Run relational queries, joins, aggregates, CTEs, windows, JSON operations, and transactions with a PostgreSQL-oriented SQL surface.
- Search text with BM25 or Bayesian BM25, retrieve vectors with KNN, and combine both signals in hybrid queries.
- Store named graphs, execute Cypher and regular path queries, and call graph traversal or centrality functions from SQL.
- Start in memory for experiments, then open a persistent SQLite-backed catalog without changing the query API.
- Embed the engine in Rust or use the Python, Node.js, and browser WASM bindings included in the workspace.

> [!NOTE]
> UQA-RS is under active development at version 0.1.0. The implementation is broad and heavily tested, but public APIs and storage formats may still evolve before a stable release.

## Mathematical foundation

[A Typed Carrier Algebra for Unified Query Execution](docs/papers/typed-carrier-algebra.md)
states the implementation-grounded theory behind UQA-RS. It distinguishes
document support, weighted relations, decorated postings, ranked views, SQL
bags, join tuples, graph context, and aggregate state while showing how they
compose through one typed planning and execution framework.

## Try it in a terminal

You need Rust 1.85 or newer and the native build tools required by Cargo dependencies.

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

`Engine::new()` keeps data in memory, while `Engine::open(path)` and `usql --db <path>` use a persistent SQLite-backed catalog. Persistent catalogs restore schemas, documents, indexes, graphs, scoring parameters, models, views, and statistics when reopened.

Security-sensitive deployments should use the SQLCipher path exposed by `Engine::open_encrypted`. Compressed encrypted containers are also available when compression is required, but they have a narrower, explicitly documented threat model and require an external trusted anchor for whole-file rollback detection.

Read the [compressed VFS security contract](docs/design/compressed-vfs-security.md) before selecting that format.

## Language bindings

| Environment | Workspace package | Notes |
| --- | --- | --- |
| Rust | [`uqa-engine`](crates/uqa-engine) | Direct embedded API and runnable examples |
| Python | [`uqa-python`](crates/uqa-python) | pyo3/maturin bindings for SQL, retrieval, graph, calibration, and extensions |
| Node.js | [`uqa-node`](crates/uqa-node) | Node-API bindings with asynchronous query and search methods |
| Browser | [`uqa-wasm`](crates/uqa-wasm) | Emscripten build with SQLite persistence mounted on IndexedDB |

## Build and test

Build the complete workspace:

```sh
cargo build --workspace --locked
```

Run the test suite:

```sh
cargo test --workspace --locked
```

Contributor checks, benchmark build gates, and repository conventions are documented in [CONTRIBUTING.md](CONTRIBUTING.md).

## Documentation

| Document | Use it for |
| --- | --- |
| [Design documentation index](docs/design/README.md) | Finding the right technical contract or architecture document |
| [System architecture](docs/design/architecture.md) | Crate boundaries, query planning, carriers, execution, storage, and extension points |
| [Vector indexes](docs/design/vector-indexes.md) | Brute-force, IVF, and HNSW behavior, parameters, persistence, and correctness contracts |
| [Engine state ownership](docs/design/engine-state-ownership.md) | Session isolation, locks, epochs, and publication rules |
| [Compressed VFS security](docs/design/compressed-vfs-security.md) | Encryption format, authenticated metadata, rollback limits, and deployment guidance |
| [Performance](docs/design/performance.md) | Reproducible baselines, regression gates, bottlenecks, and benchmark limitations |
| [Parity fixtures](docs/design/parity.md) | SQL, relevance, and vector-calibration compatibility fixtures |
| [Licensing policy](https://github.com/cognica-io/uqa-rs/blob/main/LICENSING.md) | AGPL, FOSS, noncommercial, commercial, and contribution paths |
| [History](HISTORY.md) | Release-by-release changes |

## Project layout

The repository is a Rust workspace with small crates for the algebra, storage, scoring, graph, SQL, planning, execution, engine, CLI, APIs, and language bindings. The full dependency map and ownership rules live in the [system architecture](docs/design/architecture.md), keeping this README focused on using the project.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local gates, test conventions, crate boundaries, pull request guidelines, and the current contributor-licensing requirement.

## License

UQA-RS is open-source software licensed under AGPL-3.0-only. See
[LICENSE](https://github.com/cognica-io/uqa-rs/blob/main/LICENSE).

Two optional additional permissions are available:

- the [FOSS exception](https://github.com/cognica-io/uqa-rs/blob/main/LICENSES/UQA-FOSS-EXCEPTION-1.0.txt) lets a complete
  qualifying open-source application retain its OSI-approved license while
  UQA-RS and modifications to UQA-RS remain under the AGPL; and
- the [noncommercial application exception](https://github.com/cognica-io/uqa-rs/blob/main/LICENSES/UQA-NONCOMMERCIAL-EXCEPTION-1.0.txt)
  lets a qualifying personal, educational, academic, or charitable
  application keep its independent code under terms chosen by its author
  while UQA-RS and modifications to UQA-RS remain under the AGPL.

Separate [commercial licensing](https://github.com/cognica-io/uqa-rs/blob/main/COMMERCIAL.md) is available for proprietary
applications, closed modifications, SaaS, and OEM distribution. The complete
decision guide is in the [licensing policy](https://github.com/cognica-io/uqa-rs/blob/main/LICENSING.md).
