# Tutorial 2: Full-Text Search

This tutorial adds indexed text to the issue catalog, runs BM25 retrieval, combines ranked and relational predicates, and inspects deterministic ordering.

## 1. Open the tutorial database

```sh
cargo run -p uqa-cli --bin usql -- --db tutorial.uqa
```

If Tutorial 1 was not run, create the table used here:

```sql
CREATE TABLE projects (
    project_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE issues (
    issue_id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    priority INTEGER NOT NULL CHECK (priority BETWEEN 1 AND 5),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (project_id) REFERENCES projects(project_id)
        ON DELETE CASCADE
);

INSERT INTO projects (project_id, name) VALUES (1, 'manual');

INSERT INTO issues (
    issue_id, project_id, title, body, status, priority
) VALUES
    (101, 1, 'Write SQL reference', '', 'open', 2),
    (102, 1, 'Verify tutorial examples', '', 'open', 1),
    (103, 1, 'Render architecture diagram', '', 'open', 3);
```

When the table already exists without `body`, add it:

```sql
ALTER TABLE issues ADD COLUMN body TEXT DEFAULT '';
```

## 2. Load searchable content

```sql
UPDATE issues
SET body = CASE issue_id
    WHEN 101 THEN 'Document supported SQL statements and database constraints'
    WHEN 102 THEN 'Execute and verify every tutorial SQL example'
    WHEN 103 THEN 'Explain query planning storage and graph execution'
    ELSE body
END;

INSERT INTO issues (
    issue_id, project_id, title, body, status, priority
) VALUES
    (104, 1, 'Tune ranked retrieval', 'Measure ranked retrieval with BM25 and vector recall', 'open', 2),
    (105, 1, 'Review browser guide', 'Explain WASM persistence with IndexedDB', 'open', 3);
```

## 3. Create a GIN text index

```sql
CREATE INDEX issues_text_gin
ON issues USING gin (title, body);
```

A GIN index can cover multiple text fields. Search functions validate that the requested field is indexed.

Inspect it with:

```text
\di
```

## 4. Run a BM25 query

```sql
SELECT issue_id, title, _score
FROM issues
WHERE text_match(body, 'SQL tutorial')
ORDER BY _score DESC, issue_id ASC
LIMIT 10;
```

`text_match` analyzes the query, retrieves postings, computes BM25, and exposes the ranking as `_score`. The secondary key makes ties repeatable.

## 5. Use Boolean and phrase grammar

Use `fts_match` for explicit query syntax:

```sql
SELECT issue_id, title, _score
FROM issues
WHERE fts_match(body, '(SQL OR query) AND NOT browser')
ORDER BY _score DESC, issue_id ASC;
```

Search an exact phrase:

```sql
SELECT issue_id, title, _score
FROM issues
WHERE fts_match(body, '"ranked retrieval"')
ORDER BY _score DESC, issue_id ASC;
```

The precedence order is `NOT`, `AND`, then `OR`; adjacent terms imply `AND`.

## 6. Combine rank with relational filters

```sql
SELECT issue_id, title, status, _score
FROM issues
WHERE text_match(body, 'SQL query')
  AND status = 'open'
ORDER BY _score DESC, issue_id ASC
LIMIT 5;
```

The planner combines retrieval support with residual predicates. Keep the final `LIMIT` close to the requested result count, and use a deterministic tie key.

## 7. Compare Bayesian scoring

```sql
SELECT issue_id, title, _score
FROM issues
WHERE bayesian_match(body, 'SQL query')
ORDER BY _score DESC, issue_id ASC;
```

Bayesian BM25 maps the complete raw query score through learned or estimated calibration parameters. Treat the value as a calibrated probability only after evaluating representative held out labels.

## 8. Profile the typed search path

```rust
use uqa_core::Value;
use uqa_engine::{Engine, ScoringMode};
use uqa_storage::document_store::Document;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();
    engine.create_default_table("notes", vec!["body".into()])?;

    let mut first = Document::new();
    first.insert("body".into(), Value::Str("rust query engine".into()));
    engine.add_document("notes", 1, first)?;

    let mut second = Document::new();
    second.insert("body".into(), Value::Str("browser rendering".into()));
    engine.add_document("notes", 2, second)?;

    let profile = engine.search_profiled(
        "notes",
        "body",
        "rust query",
        &ScoringMode::default(),
        10,
    )?;
    println!("{profile:?}");
    Ok(())
}
```

The profile reports the selected text top-K algorithm and physical counters. Use it to investigate execution behavior, not as a substitute for relevance labels.

Continue with [Analyzer pipelines](03-analyzer-pipelines.md) to customize tokenization and normalization, or move to [Vector and hybrid search](04-vector-and-hybrid-search.md).
