# Quick Start

## Requirements

- Rust 1.90 or newer
- Cargo and the native build tools required by workspace dependencies
- A checkout of this repository

Run commands from the workspace root.

## Start an in-memory SQL shell

```sh
cargo run -p uqa-cli --bin usql
```

At the prompt, create a table, a full-text index, and three rows:

```sql
CREATE TABLE notes (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    body TEXT NOT NULL
);

CREATE INDEX notes_body_gin ON notes USING gin (body);

INSERT INTO notes (id, title, body) VALUES
    (1, 'Rust async', 'Futures and the Tokio runtime'),
    (2, 'Embedded Rust', 'Drivers for constrained devices'),
    (3, 'Web application', 'Forms, routing, and templates');
```

Run relational SQL:

```sql
SELECT id, title
FROM notes
WHERE id >= 2
ORDER BY id;
```

Run ranked text retrieval:

```sql
SELECT id, title, _score
FROM notes
WHERE text_match(body, 'rust async')
ORDER BY _score DESC, id ASC
LIMIT 5;
```

The `gin` index is required for text retrieval. `_score` is a virtual value produced by retrieval operators and is not a stored table column.

Exit with `\q`.

## Use a persistent database

Pass a path with `--db`:

```sh
cargo run -p uqa-cli --bin usql -- --db notes.uqa
```

`Engine::open` and `usql --db` use the SQLite-backed persistent format by default. Reopen the same path to restore schemas, rows, indexes, graphs, views, statistics, models, and durable engine parameters.

Run one command without entering the shell:

```sh
cargo run -p uqa-cli --bin usql -- --db notes.uqa -c "SELECT count(*) FROM notes"
```

## Embed the engine in Rust

Use the `uqa` facade package for the embedded API. It re-exports `uqa-engine` and the core `Value` type; depend directly on `uqa-engine` only when the implementation package name is required. The embedded API has the same SQL behavior as the CLI.

```rust
use uqa::{Engine, SQLParam, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();

    engine.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
        &[],
    )?;
    engine.sql(
        "INSERT INTO notes (id, body) VALUES ($1, $2)",
        &[
            SQLParam::scalar(Value::Int(1)),
            SQLParam::scalar(Value::Str("hello from UQA-RS".into())),
        ],
    )?;

    let result = engine.sql(
        "SELECT id, body FROM notes WHERE id = $1",
        &[SQLParam::scalar(Value::Int(1))],
    )?;

    for row in result.rows {
        println!("{row:?}");
    }

    Ok(())
}
```

For the exact parameter and result contracts, continue with the [Rust engine API](02-rust-engine-api.md).

## Run repository examples

```sh
cargo run -p uqa-engine --example text_search
cargo run -p uqa-engine --example hybrid_search
cargo run -p uqa-engine --example sqlcipher_encrypted_catalog
cargo run -p uqa-engine --example compressed_encrypted_catalog
```

The [example matrix](../../../examples/README.md) provides matching unified-search, vector-KNN, graph/Cypher, storage/transaction, and extensibility programs for Rust, Python, Node.js, and Browser WASM.

## Next steps

- Follow [Your first database](../tutorials/01-first-database.md) for a complete schema and transaction workflow.
- Follow [Full-text search](../tutorials/02-full-text-search.md) to learn query grammar and ranking.
- Follow [Analyzer pipelines](../tutorials/03-analyzer-pipelines.md) to define field-specific tokenization, normalization, and synonyms.
- Read [Supported SQL](../sql/README.md) when porting an existing SQL workload.
