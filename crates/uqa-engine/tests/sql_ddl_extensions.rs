//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the DDL surface added to match the UQA
//! engine: `BIGSERIAL` / `SERIAL` columns with auto-id INSERTs,
//! `DROP TABLE / INDEX [IF EXISTS]`, and `ALTER TABLE` action variants
//! (ADD / DROP / RENAME COLUMN, RENAME TO).

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

fn sqlite_count(db: &std::path::Path, sql: &str, params: &[&str]) -> i64 {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    match params {
        [a] => stmt.query_row([*a], |row| row.get(0)).unwrap(),
        [a, b] => stmt.query_row([*a, *b], |row| row.get(0)).unwrap(),
        _ => stmt.query_row([], |row| row.get(0)).unwrap(),
    }
}

fn ivf_metadata_count(db: &std::path::Path, table: &str, field: &str) -> i64 {
    sqlite_count(
        db,
        "SELECT COUNT(*) FROM _ivf_indexes WHERE table_name = ?1 AND field = ?2",
        &[table, field],
    )
}

fn vector_row_count(db: &std::path::Path, table: &str, field: &str) -> i64 {
    sqlite_count(
        db,
        "SELECT COUNT(*) FROM _vectors WHERE table_name = ?1 AND field = ?2",
        &[table, field],
    )
}

fn catalog_index_table(db: &std::path::Path, name: &str) -> Option<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row(
        "SELECT table_name FROM _catalog_indexes WHERE name = ?1",
        [name],
        |row| row.get(0),
    )
    .ok()
}

fn catalog_index_columns(db: &std::path::Path, name: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let json: Option<String> = conn
        .query_row(
            "SELECT columns FROM _catalog_indexes WHERE name = ?1",
            [name],
            |row| row.get(0),
        )
        .ok();
    json.and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

fn field_stats_total(db: &std::path::Path, table: &str, field: &str) -> Option<i64> {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row(
        "SELECT total_length FROM _field_stats WHERE table_name = ?1 AND field = ?2",
        [table, field],
        |row| row.get(0),
    )
    .ok()
}

#[test]
fn bigserial_auto_id_assigns_monotonic_ids() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE messages (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO messages (body) VALUES ('first')", &[])
        .unwrap();
    eng.sql("INSERT INTO messages (body) VALUES ('second')", &[])
        .unwrap();
    let res = eng
        .sql("SELECT id, body FROM messages ORDER BY id", &[])
        .unwrap();
    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0]["id"], Value::Int(1));
    assert_eq!(res.rows[0]["body"], Value::Str("first".into()));
    assert_eq!(res.rows[1]["id"], Value::Int(2));
    assert_eq!(res.rows[1]["body"], Value::Str("second".into()));
}

#[test]
fn serial_auto_id_with_explicit_id_advances_watermark() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id SERIAL PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id, body) VALUES (10, 'jump')", &[])
        .unwrap();
    eng.sql("INSERT INTO t (body) VALUES ('after-jump')", &[])
        .unwrap();
    let res = eng.sql("SELECT id FROM t ORDER BY id", &[]).unwrap();
    let ids: Vec<i64> = res
        .rows
        .iter()
        .map(|r| match r.get("id").expect("id column") {
            Value::Int(v) => *v,
            other => panic!("expected int, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![10, 11]);
}

#[test]
fn drop_table_if_exists_is_noop_when_missing() {
    let eng = Engine::new();
    eng.sql("DROP TABLE IF EXISTS does_not_exist", &[]).unwrap();
}

#[test]
fn drop_table_without_if_exists_errors_when_missing() {
    let eng = Engine::new();
    let err = eng.sql("DROP TABLE missing", &[]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("DROP TABLE"), "unexpected error: {msg}");
}

#[test]
fn drop_index_if_exists_is_noop() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("DROP INDEX IF EXISTS notes_body_idx", &[]).unwrap();
}

#[test]
fn drop_index_without_if_exists_errors_when_missing() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    let err = eng.sql("DROP INDEX notes_body_idx", &[]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("DROP INDEX"), "unexpected error: {msg}");
}

#[test]
fn alter_table_add_column_and_insert_uses_it() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("ALTER TABLE notes ADD COLUMN tag TEXT", &[])
        .unwrap();
    eng.sql("INSERT INTO notes (body, tag) VALUES ('hi', 'greet')", &[])
        .unwrap();
    let res = eng
        .sql("SELECT id, body, tag FROM notes ORDER BY id", &[])
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0]["tag"], Value::Str("greet".into()));
}

#[test]
fn alter_table_drop_column_removes_visibility() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT, tag TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (body, tag) VALUES ('payload', 'first')",
        &[],
    )
    .unwrap();
    eng.sql("ALTER TABLE notes DROP COLUMN tag", &[]).unwrap();
    assert!(!eng.table_has_column("notes", "tag"));
}

#[test]
fn alter_table_rename_column_propagates() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("ALTER TABLE notes RENAME COLUMN body TO content", &[])
        .unwrap();
    assert!(eng.table_has_column("notes", "content"));
    assert!(!eng.table_has_column("notes", "body"));
}

#[test]
fn bigserial_watermark_survives_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("uqa.db");
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            "CREATE TABLE messages (id BIGSERIAL PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
        eng.sql("INSERT INTO messages (body) VALUES ('a')", &[])
            .unwrap();
        eng.sql("INSERT INTO messages (body) VALUES ('b')", &[])
            .unwrap();
    }
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql("INSERT INTO messages (body) VALUES ('c')", &[])
            .unwrap();
        let res = eng.sql("SELECT id FROM messages ORDER BY id", &[]).unwrap();
        let ids: Vec<i64> = res
            .rows
            .iter()
            .map(|r| match r.get("id").expect("id") {
                Value::Int(v) => *v,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }
}

#[test]
fn alter_table_rename_table_moves_state() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO notes (body) VALUES ('hello')", &[])
        .unwrap();
    eng.sql("ALTER TABLE notes RENAME TO posts", &[]).unwrap();
    assert!(eng.has_table("posts"));
    assert!(!eng.has_table("notes"));
    let res = eng.sql("SELECT body FROM posts ORDER BY id", &[]).unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0]["body"], Value::Str("hello".into()));
}

#[test]
fn persistent_rename_table_moves_rows_indexes_and_ivf_metadata() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("uqa.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO docs (id, body, embedding) VALUES \
             (1, 'hello world', ARRAY[1.0, 0.0]), \
             (2, 'other', ARRAY[0.0, 1.0]), \
             (3, 'hello vector', ARRAY[0.8, 0.2])",
            &[],
        )
        .unwrap();
        eng.sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
            .unwrap();
        eng.sql(
            "CREATE INDEX docs_embedding_ivf ON docs USING ivf (embedding) \
             WITH (lists = 2, probes = 1, train_threshold = 2)",
            &[],
        )
        .unwrap();

        assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 1);
        eng.sql("ALTER TABLE docs RENAME TO posts", &[]).unwrap();
        assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 0);
        assert_eq!(ivf_metadata_count(&db, "posts", "embedding"), 1);
        assert_eq!(
            catalog_index_table(&db, "docs_body_gin").as_deref(),
            Some("posts")
        );
        assert_eq!(
            catalog_index_table(&db, "docs_embedding_ivf").as_deref(),
            Some("posts")
        );

        let rows = eng
            .sql(
                "SELECT id FROM posts WHERE text_match(body, 'hello') ORDER BY id",
                &[],
            )
            .unwrap();
        assert_eq!(rows.rows.len(), 2);
        let hits = eng.knn_search("posts", "embedding", vec![1.0, 0.0], 1);
        assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
    }

    let reopened = Engine::open(&db).unwrap();
    assert!(reopened.has_table("posts"));
    assert!(!reopened.has_table("docs"));
    assert_eq!(ivf_metadata_count(&db, "posts", "embedding"), 1);
    let rows = reopened
        .sql(
            "SELECT id FROM posts WHERE text_match(body, 'hello') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    let hits = reopened.knn_search("posts", "embedding", vec![1.0, 0.0], 1);
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
}

#[test]
fn persistent_drop_column_removes_dependent_index_metadata() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("uqa.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO docs (id, body, embedding) VALUES \
             (1, 'hello', ARRAY[1.0, 0.0]), \
             (2, 'other', ARRAY[0.0, 1.0])",
            &[],
        )
        .unwrap();
        eng.sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
            .unwrap();
        eng.sql(
            "CREATE INDEX docs_embedding_ivf ON docs USING ivf (embedding) \
             WITH (lists = 2, probes = 1, train_threshold = 2)",
            &[],
        )
        .unwrap();
        eng.sql("ALTER TABLE docs DROP COLUMN embedding", &[])
            .unwrap();

        assert!(eng.has_catalog_index("docs_body_gin"));
        assert!(!eng.has_catalog_index("docs_embedding_ivf"));
        assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 0);
        assert!(catalog_index_table(&db, "docs_embedding_ivf").is_none());
    }

    let reopened = Engine::open(&db).unwrap();
    assert!(reopened.has_catalog_index("docs_body_gin"));
    assert!(!reopened.has_catalog_index("docs_embedding_ivf"));
    assert!(!reopened.table_has_column("docs", "embedding"));
    assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 0);
}

#[test]
fn persistent_rename_column_updates_dependent_index_metadata() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("uqa.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO docs (id, body, embedding) VALUES \
             (1, 'hello world', ARRAY[1.0, 0.0]), \
             (2, 'other', ARRAY[0.0, 1.0]), \
             (3, 'hello vector', ARRAY[0.8, 0.2])",
            &[],
        )
        .unwrap();
        eng.sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
            .unwrap();
        eng.sql(
            "CREATE INDEX docs_embedding_ivf ON docs USING ivf (embedding) \
             WITH (lists = 2, probes = 1, train_threshold = 2)",
            &[],
        )
        .unwrap();

        eng.sql("ALTER TABLE docs RENAME COLUMN body TO content", &[])
            .unwrap();
        eng.sql("ALTER TABLE docs RENAME COLUMN embedding TO vector", &[])
            .unwrap();

        assert_eq!(catalog_index_columns(&db, "docs_body_gin"), vec!["content"]);
        assert_eq!(
            catalog_index_columns(&db, "docs_embedding_ivf"),
            vec!["vector"]
        );
        assert_eq!(field_stats_total(&db, "docs", "content"), Some(5));
        assert_eq!(field_stats_total(&db, "docs", "body"), None);
        assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 0);
        assert_eq!(ivf_metadata_count(&db, "docs", "vector"), 1);
        let rows = eng
            .sql(
                "SELECT id FROM docs WHERE text_match(content, 'hello') ORDER BY id",
                &[],
            )
            .unwrap();
        assert_eq!(rows.rows.len(), 2);
        let hits = eng.knn_search("docs", "vector", vec![1.0, 0.0], 1);
        assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
    }

    let reopened = Engine::open(&db).unwrap();
    assert!(reopened.table_has_column("docs", "content"));
    assert!(reopened.table_has_column("docs", "vector"));
    assert_eq!(catalog_index_columns(&db, "docs_body_gin"), vec!["content"]);
    assert_eq!(
        catalog_index_columns(&db, "docs_embedding_ivf"),
        vec!["vector"]
    );
    assert_eq!(field_stats_total(&db, "docs", "content"), Some(5));
    assert_eq!(field_stats_total(&db, "docs", "body"), None);
    assert_eq!(ivf_metadata_count(&db, "docs", "vector"), 1);
    let rows = reopened
        .sql(
            "SELECT id FROM docs WHERE text_match(content, 'hello') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    let hits = reopened.knn_search("docs", "vector", vec![1.0, 0.0], 1);
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
}

#[test]
fn persistent_alter_vector_column_to_text_drops_ivf_metadata() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("uqa.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO docs (id, embedding) VALUES \
             (1, ARRAY[1.0, 0.0]), \
             (2, ARRAY[0.0, 1.0]), \
             (3, ARRAY[0.8, 0.2])",
            &[],
        )
        .unwrap();
        eng.sql(
            "CREATE INDEX docs_embedding_ivf ON docs USING ivf (embedding) \
             WITH (lists = 2, probes = 1, train_threshold = 2)",
            &[],
        )
        .unwrap();
        assert!(eng.has_catalog_index("docs_embedding_ivf"));
        assert_eq!(vector_row_count(&db, "docs", "embedding"), 3);
        assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 1);

        eng.sql("ALTER TABLE docs ALTER COLUMN embedding TYPE TEXT", &[])
            .unwrap();

        assert!(!eng.has_catalog_index("docs_embedding_ivf"));
        assert_eq!(vector_row_count(&db, "docs", "embedding"), 0);
        assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 0);
        assert!(catalog_index_table(&db, "docs_embedding_ivf").is_none());
        assert!(eng
            .knn_search("docs", "embedding", vec![1.0, 0.0], 1)
            .is_empty());
    }

    let reopened = Engine::open(&db).unwrap();
    assert!(!reopened.has_catalog_index("docs_embedding_ivf"));
    assert_eq!(vector_row_count(&db, "docs", "embedding"), 0);
    assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 0);
    assert!(reopened
        .knn_search("docs", "embedding", vec![1.0, 0.0], 1)
        .is_empty());
}
