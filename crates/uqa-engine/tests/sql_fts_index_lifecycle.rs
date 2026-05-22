//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regression coverage for SQL-managed FTS indexes: `CREATE INDEX USING gin`
//! must build postings for existing rows, analyzer options must affect the
//! real index backend, and `CREATE TABLE` must not auto-index every TEXT
//! column.

use std::path::Path;

use rusqlite::params;
use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::{document_store::Document, ManagedConnection};

fn ids(result: &uqa_sql::SQLResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(id)) => *id,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect()
}

fn int_col(row: &uqa_sql::ResultRow, name: &str) -> i64 {
    match row.get(name) {
        Some(Value::Int(value)) => *value,
        other => panic!("expected integer column {name}, got {other:?}"),
    }
}

fn create_notes_gin_fixture(db: &Path) {
    let eng = Engine::open(db).unwrap();
    eng.sql(
        "CREATE TABLE notes (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            status TEXT NOT NULL
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (id, title, content, status) VALUES
         ('note:a', 'Local Learning', 'Bayesian local learning notes', 'indexed'),
         ('note:b', 'Runtime Systems', 'Executor implementation notes', 'indexed')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX notes_search_gin ON notes USING gin (title, content)
         WITH (analyzer = 'standard_cjk')",
        &[],
    )
    .unwrap();
}

fn rewrite_fts_tables_to_legacy_shape(db: &Path) {
    let conn = ManagedConnection::open(db).unwrap();
    conn.with(|c| {
        c.execute("DROP TABLE _postings", [])?;
        c.execute("DROP TABLE _doc_lengths", [])?;
        c.execute("DROP TABLE _field_stats", [])?;
        c.execute(
            "CREATE TABLE _postings (
                table_name TEXT NOT NULL,
                field      TEXT NOT NULL,
                term       TEXT NOT NULL,
                doc_id     INTEGER NOT NULL,
                positions  TEXT NOT NULL,
                PRIMARY KEY (table_name, field, term, doc_id)
            )",
            [],
        )?;
        c.execute(
            "CREATE TABLE _doc_lengths (
                table_name TEXT NOT NULL,
                doc_id     INTEGER NOT NULL,
                lengths    TEXT NOT NULL,
                PRIMARY KEY (table_name, doc_id)
            )",
            [],
        )?;
        c.execute(
            "CREATE TABLE _field_stats (
                table_name   TEXT NOT NULL,
                field        TEXT NOT NULL,
                total_length INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (table_name, field)
            )",
            [],
        )?;
        Ok(())
    })
    .unwrap();
}

fn assert_legacy_gin_reopens_with_restored_index(db: &Path) {
    let eng = Engine::open(db).unwrap();
    let stats = eng
        .sql(
            "SELECT field, analyzer, doc_length_count
             FROM fts_index_stats('notes')
             ORDER BY field",
            &[],
        )
        .unwrap();
    assert_eq!(stats.rows.len(), 2);
    for row in &stats.rows {
        assert_eq!(row["analyzer"], Value::Str("standard_cjk".into()));
        assert_eq!(int_col(row, "doc_length_count"), 2);
    }

    let hits = eng
        .sql(
            "SELECT id FROM notes
             WHERE multi_field_match(title, content, 'Learning', 2.0, 1.0)
               AND status = 'indexed'
             ORDER BY id",
            &[],
        )
        .unwrap();
    let learning_ids: Vec<String> = hits
        .rows
        .iter()
        .filter_map(|row| match row.get("id") {
            Some(Value::Str(id)) => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert!(
        learning_ids.iter().any(|id| id == "note:a"),
        "expected note:a in hits, got {learning_ids:?}"
    );
}

fn overwrite_document_body(db: &Path, table: &str, doc_id: i64, document: &Document) {
    let body = serde_json::to_string(&document).unwrap();
    let conn = ManagedConnection::open(db).unwrap();
    conn.with(|c| {
        c.execute(
            "UPDATE _documents SET body = ?1 WHERE table_name = ?2 AND doc_id = ?3",
            params![body, table, doc_id],
        )?;
        Ok(())
    })
    .unwrap();
}

fn delete_ivf_metadata(db: &Path, table: &str, field: &str) {
    let conn = ManagedConnection::open(db).unwrap();
    conn.with(|c| {
        c.execute(
            "DELETE FROM _ivf_indexes WHERE table_name = ?1 AND field = ?2",
            params![table, field],
        )?;
        c.execute(
            "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
            params![table, field],
        )?;
        c.execute(
            "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
            params![table, field],
        )?;
        Ok(())
    })
    .unwrap();
}

fn ivf_metadata_count(db: &Path, table: &str, field: &str) -> i64 {
    let conn = ManagedConnection::open(db).unwrap();
    conn.with(|c| {
        let n = c.query_row(
            "SELECT COUNT(*) FROM _ivf_indexes WHERE table_name = ?1 AND field = ?2",
            params![table, field],
            |r| r.get(0),
        )?;
        Ok(n)
    })
    .unwrap()
}

#[test]
fn gin_index_backfills_existing_rows_and_does_not_auto_index_other_text_columns() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE messages (id INTEGER PRIMARY KEY, content TEXT, context_json TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        r#"INSERT INTO messages (id, content, context_json) VALUES
           (1, '대구호텔 예약', '{"shadow":"대구호텔"}'),
           (2, '서울 카페', '{"shadow":"대구호텔"}')"#,
        &[],
    )
    .unwrap();

    let before_stats = eng
        .sql("SELECT * FROM fts_index_stats('messages')", &[])
        .unwrap();
    assert!(before_stats.rows.is_empty());
    let before_search = eng
        .sql(
            "SELECT id FROM messages WHERE text_match(content, '호텔') ORDER BY id",
            &[],
        )
        .unwrap();
    assert!(before_search.rows.is_empty());

    eng.sql(
        "CREATE INDEX idx_messages_content_gin ON messages USING gin (content) \
         WITH (analyzer = 'standard_cjk')",
        &[],
    )
    .unwrap();

    let content_hits = eng
        .sql(
            "SELECT id FROM messages WHERE text_match(content, '호텔') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(ids(&content_hits), vec![1]);

    let metadata_hits = eng
        .sql(
            "SELECT id FROM messages WHERE text_match(context_json, '대구호텔') ORDER BY id",
            &[],
        )
        .unwrap();
    assert!(metadata_hits.rows.is_empty());

    let stats = eng
        .sql(
            "SELECT field, analyzer, posting_count, doc_length_count, indexed_doc_count, term_count \
             FROM fts_index_stats('messages')",
            &[],
        )
        .unwrap();
    assert_eq!(stats.rows.len(), 1);
    assert_eq!(stats.rows[0]["field"], Value::Str("content".into()));
    assert_eq!(stats.rows[0]["analyzer"], Value::Str("standard_cjk".into()));
    assert!(int_col(&stats.rows[0], "posting_count") > 0);
    assert_eq!(int_col(&stats.rows[0], "doc_length_count"), 2);
    assert_eq!(int_col(&stats.rows[0], "indexed_doc_count"), 2);
    assert!(int_col(&stats.rows[0], "term_count") > 0);
}

#[test]
fn gin_index_backfills_existing_rows_with_text_primary_key() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("uqa.db");
    let eng = Engine::open(&db).unwrap();
    eng.sql(
        "CREATE TABLE notes (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            status TEXT NOT NULL
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (id, title, content, status) VALUES
         ('note:a', 'Local Learning', 'Bayesian local learning notes', 'indexed'),
         ('note:b', 'Runtime Systems', 'Executor implementation notes', 'indexed')",
        &[],
    )
    .unwrap();

    eng.sql(
        "CREATE INDEX notes_search_gin ON notes USING gin (title, content)
         WITH (analyzer = 'standard_cjk')",
        &[],
    )
    .unwrap();

    let hits = eng
        .sql(
            "SELECT id, _score FROM notes
             WHERE multi_field_match(title, content, 'Learning', 2.0, 1.0)
               AND status = 'indexed'
             ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    let ids: Vec<String> = hits
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Str(id)) => id.clone(),
            other => panic!("expected string id, got {other:?}"),
        })
        .collect();
    assert!(
        ids.iter().any(|id| id == "note:a"),
        "expected note:a in hits, got {ids:?}"
    );

    let stats = eng
        .sql(
            "SELECT field, analyzer, doc_length_count, indexed_doc_count
             FROM fts_index_stats('notes')
             ORDER BY field",
            &[],
        )
        .unwrap();
    assert_eq!(stats.rows.len(), 2);
    for row in &stats.rows {
        assert_eq!(row["analyzer"], Value::Str("standard_cjk".into()));
        assert_eq!(int_col(row, "doc_length_count"), 2);
        assert_eq!(int_col(row, "indexed_doc_count"), 2);
    }
}

#[test]
fn catalog_open_upgrades_legacy_fts_storage_shape_before_restoring_gin() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("uqa.db");
    create_notes_gin_fixture(&db);
    rewrite_fts_tables_to_legacy_shape(&db);
    assert_legacy_gin_reopens_with_restored_index(&db);
}

#[test]
fn gin_analyzer_assignment_persists_and_indexes_new_rows_after_reopen() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("uqa.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE messages (id INTEGER PRIMARY KEY, content TEXT)",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO messages (id, content) VALUES (1, '대구호텔 예약')",
            &[],
        )
        .unwrap();
        eng.sql(
            "CREATE INDEX idx_messages_content_gin ON messages USING gin (content) \
             WITH (analyzer = 'standard_cjk')",
            &[],
        )
        .unwrap();
    }
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "INSERT INTO messages (id, content) VALUES (2, '부산호텔 추천')",
            &[],
        )
        .unwrap();
        let hits = eng
            .sql(
                "SELECT id FROM messages WHERE text_match(content, '호텔') ORDER BY id",
                &[],
            )
            .unwrap();
        assert_eq!(ids(&hits), vec![1, 2]);

        let stats = eng
            .sql(
                "SELECT analyzer, doc_length_count FROM fts_index_stats('messages')",
                &[],
            )
            .unwrap();
        assert_eq!(stats.rows.len(), 1);
        assert_eq!(stats.rows[0]["analyzer"], Value::Str("standard_cjk".into()));
        assert_eq!(int_col(&stats.rows[0], "doc_length_count"), 2);
    }
}

#[test]
fn reopen_reuses_persisted_gin_postings_without_rebuilding() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("uqa.db");
    {
        let eng = Engine::open(&db).unwrap();
        eng.sql(
            "CREATE TABLE messages (id INTEGER PRIMARY KEY, content TEXT)",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO messages (id, content) VALUES (1, 'alpha token')",
            &[],
        )
        .unwrap();
        eng.sql(
            "CREATE INDEX idx_messages_content_gin ON messages USING gin (content)",
            &[],
        )
        .unwrap();
    }

    let mut replacement = Document::new();
    replacement.insert("content".into(), Value::Str("beta token".into()));
    overwrite_document_body(&db, "messages", 1, &replacement);

    let eng = Engine::open(&db).unwrap();
    let alpha = eng
        .sql(
            "SELECT COUNT(*) AS n FROM messages WHERE text_match(content, 'alpha')",
            &[],
        )
        .unwrap();
    let beta = eng
        .sql(
            "SELECT COUNT(*) AS n FROM messages WHERE text_match(content, 'beta')",
            &[],
        )
        .unwrap();
    assert_eq!(int_col(&alpha.rows[0], "n"), 1);
    assert_eq!(int_col(&beta.rows[0], "n"), 0);
}

#[test]
fn reopen_attaches_ivf_index_without_bootstrap_retraining() {
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
    }
    assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 1);
    delete_ivf_metadata(&db, "docs", "embedding");
    assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 0);

    {
        let _eng = Engine::open(&db).unwrap();
        assert_eq!(ivf_metadata_count(&db, "docs", "embedding"), 0);
    }
}
