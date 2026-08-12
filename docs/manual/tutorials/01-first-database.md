# Tutorial 1: Your First Database

This tutorial creates a persistent issue catalog, applies relational constraints, performs a transaction, and queries the result.

## 1. Open a persistent shell

```sh
cargo run -p uqa-cli --bin usql -- --db tutorial.uqa
```

The file is created with the default SQLite-backed format. Run `\where` to confirm the active location.

## 2. Create the schema

```sql execute
CREATE TABLE projects (
    project_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE issues (
    issue_id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    priority INTEGER NOT NULL CHECK (priority BETWEEN 1 AND 5),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (project_id) REFERENCES projects(project_id)
        ON DELETE CASCADE
);
```

Inspect the catalog:

```text
\dt
\d projects
\d issues
```

The primary keys reject duplicate identities. `NOT NULL`, `UNIQUE`, `CHECK`, `DEFAULT`, and the foreign key are enforced during mutation.

## 3. Insert related rows atomically

```sql execute
BEGIN;

INSERT INTO projects (project_id, name)
VALUES (1, 'manual');

INSERT INTO issues (issue_id, project_id, title, priority) VALUES
    (101, 1, 'Write SQL reference', 2),
    (102, 1, 'Verify tutorial examples', 1),
    (103, 1, 'Render architecture diagram', 3);

COMMIT;
```

The two tables publish together at commit. If any statement fails before `COMMIT`, issue `ROLLBACK` and correct the input.

## 4. Query, aggregate, and order

```sql execute
SELECT issue_id, title, status, priority
FROM issues
WHERE status = 'open'
ORDER BY priority ASC, issue_id ASC;
```

Group by status:

```sql execute
SELECT status, count(*) AS issue_count, avg(priority) AS average_priority
FROM issues
GROUP BY status
ORDER BY status;
```

Join the project:

```sql execute
SELECT p.name AS project, i.issue_id, i.title
FROM projects AS p
JOIN issues AS i ON i.project_id = p.project_id
ORDER BY i.issue_id;
```

## 5. Update with `RETURNING`

```sql execute
UPDATE issues
SET status = 'closed'
WHERE issue_id = 102
RETURNING issue_id, status;
```

Verify the remaining open work:

```sql execute
SELECT issue_id, title
FROM issues
WHERE status = 'open'
ORDER BY issue_id;
```

## 6. Use a common table expression

```sql execute
WITH open_issues AS (
    SELECT project_id, priority
    FROM issues
    WHERE status = 'open'
)
SELECT p.name, count(*) AS open_count, min(o.priority) AS highest_urgency
FROM projects AS p
JOIN open_issues AS o ON o.project_id = p.project_id
GROUP BY p.name;
```

The CTE is part of the same SQL statement and participates in planning with its consumer.

## 7. Bind values from Rust

Applications should bind data instead of interpolating it into SQL text:

```rust
use std::path::Path;
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::open(Path::new("tutorial.uqa"))?;
    let result = engine.sql(
        "SELECT issue_id, title FROM issues WHERE status = $1 ORDER BY issue_id",
        &[SQLParam::scalar(Value::Str("open".into()))],
    )?;

    for row in result.rows {
        println!("{row:?}");
    }
    Ok(())
}
```

Binding preserves value types and avoids treating input as SQL syntax. Identifiers such as table names are not value parameters; choose them from a trusted allowlist or use a higher-level builder.

## 8. Reopen and verify durability

Exit with `\q`, start the same command again, and run:

```sql execute
SELECT count(*) AS projects FROM projects;
SELECT count(*) AS issues FROM issues;
```

The committed schema and rows are restored. Continue with [Full-text search](02-full-text-search.md) to index issue text.
