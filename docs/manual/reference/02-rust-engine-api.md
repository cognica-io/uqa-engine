# Rust Engine API

The `uqa-engine` crate is the main embedded interface. It owns durable storage, session-local SQL state, runtime extensions, epochs, and query execution.

## Construct an engine

| API | Result |
| --- | --- |
| `Engine::new()` | In-memory engine |
| `Engine::open(path)` | Default persistent SQLite engine |
| `Engine::detect_database_file(path)` | File format detection without opening the engine |
| `Engine::open_auto(path, key)` | Detect plain, encrypted, compressed, or compressed-encrypted SQLite formats |
| `Engine::open_encrypted(path, key)` | SQLCipher-backed persistent engine |
| `Engine::open_compressed(path, options)` | Compressed SQLite container |
| `Engine::open_compressed_encrypted(path, key, options)` | Compressed and encrypted container |
| `Engine::open_compressed_encrypted_with_anchor(...)` | Compressed and encrypted container with a trusted rollback anchor |
| `Engine::from_persistent_provider(provider)` | Engine over a `PersistentStorageProvider`, including redb |

Use `Engine::close()` when the application needs an explicit close boundary. Normal Rust ownership still closes resources when the engine is dropped.

## Execute SQL

The central method is:

```rust
pub fn sql(
    &self,
    query: &str,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError>
```

`SQLParam` accepts scalar values and also has explicit vector and tensor constructors. Positional placeholders are `$1`, `$2`, and so on.

```rust
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

let engine = Engine::new();
engine.sql(
    "CREATE TABLE items (id INTEGER PRIMARY KEY, embedding VECTOR(3))",
    &[],
)?;
engine.sql(
    "INSERT INTO items (id, embedding) VALUES ($1, $2)",
    &[
        SQLParam::scalar(Value::Int(1)),
        SQLParam::vector(vec![1.0, 0.0, 0.0]),
    ],
)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`SQLResult` contains:

- `columns`: projected column labels in order
- `rows`: compatibility rows represented as `BTreeMap<String, Value>`
- `value_at(row, column)`: positional access that distinguishes repeated labels
- `affected_rows`: the DML row count

When projected labels repeat, `SQLResult` retains the distinct final values in a positional carrier while keeping `rows` for existing named-map callers. Use `value_at`, a cursor, or the columnar path to address repeated labels by position.

## Stream results

`Engine::sql_cursor` accepts one read query and returns a row cursor. The engine uses bounded spill when needed, and the read snapshot is committed before the cursor is returned. This makes the cursor suitable for large results without holding an open storage transaction for the consumer lifetime.

`Engine::sql_columnar` consumes result batches through a callback. It is the preferred path for column-oriented consumers and export code.

```mermaid
flowchart TD
    A[SQL text and parameters] --> B{Consumer shape}
    B -->|Materialized result| C[Engine::sql]
    B -->|Row stream| D[Engine::sql_cursor]
    B -->|Column batches| E[Engine::sql_columnar]
```

## Transactions and batches

The engine exposes explicit transaction primitives:

- `begin`, `commit`, and `rollback`
- `savepoint`, `release_savepoint`, and `rollback_to_savepoint`
- SQL forms such as `BEGIN`, `SAVEPOINT`, and `COMMIT`

`Engine::transaction` executes a Rust closure as one transaction. An error or a panic rolls the transaction back; a successful closure commits it.

`Engine::sql_batch` executes a slice of SQL statement and parameter pairs in one transaction. The entire batch rolls back when any statement fails.

```rust
use uqa_core::Value;
use uqa_engine::SQLParam;

let first = [
    SQLParam::scalar(Value::Int(1)),
    SQLParam::scalar(Value::Int(100)),
];
let second = [
    SQLParam::scalar(Value::Int(2)),
    SQLParam::scalar(Value::Int(50)),
];
let statements = [
    ("INSERT INTO accounts (id, balance) VALUES ($1, $2)",
     &first[..]),
    ("INSERT INTO accounts (id, balance) VALUES ($1, $2)",
     &second[..]),
];
engine.sql_batch(&statements)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Do not issue concurrent statements through the same session while an explicit transaction is active. Create independent sessions instead.

## Sessions

`Engine::new_session()` creates a new SQL session over the same persistent provider. Each session has independent transaction state, session variables, prepared statements, statement caches, and cancellation tokens. Durable rows, catalog objects, indexes, graph data, and runtime UDF registries are shared.

Session creation is available for engines backed by one persistent provider. An engine assembled from separate persistent backends does not provide the single provider needed to create a new session.

## Document and retrieval APIs

The API also exposes typed operations that bypass SQL text:

- Table and field setup: `create_default_table`, `create_vector_field`
- Documents: `add_document`, `add_document_with_vectors`, `get_document`, `delete_document`, `document_count`
- Vectors: `add_vector`, `knn_search`, `vector_similarity_search`
- Text: `search`, `search_profiled`
- Hybrid: `hybrid_search` for exact single-prior log-odds fusion and `robust_hybrid_search` for explicitly requested positive-evidence pooling
- Calibration: Bayesian parameter fitting and calibration reports

SQL and typed operations use the same durable state and indexes. Applications can mix them, but a single transaction boundary should use one clear ownership path.

## Analyzer APIs

Persistent custom analyzers are managed with `register_named_analyzer`, `list_named_analyzers`, `set_table_field_analyzer`, `table_field_analyzer`, `get_table_analyzer`, and `drop_named_analyzer`. Compatibility aliases use the shorter create, set, and drop names. An index-time or both-phase assignment rebuilds current postings; a search-only assignment changes query analysis without a rebuild.

The `uqa-analysis` crate exposes `Analyzer`, `CharFilter`, `Tokenizer`, and `TokenFilter` for constructing and previewing pipelines directly. Its process-global registry is not a replacement for engine catalog persistence. See [Text analyzer pipelines](06-text-analyzers.md) for JSON tags, component behavior, phase resolution, SQL examples, synonyms, and failure rules.

## Graph APIs

Named graph methods cover graph creation, deletion, listing, vertex and edge mutation, Cypher execution, traversal, and path indexes. SQL can address the same graph state through `cypher`, `rpq`, and `graph_*` functions. See [Graphs](07-graphs.md).

## Runtime extensions

Register scalar, table, and aggregate functions with:

- `register_scalar_function` and `register_scalar_function_with_options`
- `register_table_function` and `register_table_function_with_options`
- `register_aggregate_function` and `register_aggregate_function_with_options`

Default options are conservative: a function is `VOLATILE` and may mutate. Use `SQLFunctionOptions::read_only(volatility)` only when the callback is truly read-only. A callback that mutates state must remain `VOLATILE`.

Runtime callbacks are not serialized to persistent storage. Register them each time the process constructs an engine. New sessions share the runtime registry of their parent engine.

## Cancellation

Each session has its own cancellation token. `cancel()` requests cancellation, `is_cancelled()` observes it, and the reset API clears the request before later work. Long-running execution paths poll the token at safe boundaries.

Cancellation is cooperative. The caller must still handle the returned error and decide whether an explicit transaction should be rolled back.

## QueryBuilder

The `uqa-api` crate provides `QueryBuilder` for fluent construction of select, filter, retrieval, graph, aggregate, facet, fusion, and model expressions. Use it when programmatic composition is clearer than assembling SQL text. Always bind user data or use builder methods that quote values; do not interpolate untrusted strings into raw fragments.

See [Bindings and extensions](08-bindings-and-extensions.md) for a broader API matrix.
