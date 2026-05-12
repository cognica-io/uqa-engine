//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regression coverage for SQL-managed FTS indexes: `CREATE INDEX USING gin`
//! must build postings for existing rows, analyzer options must affect the
//! real index backend, and `CREATE TABLE` must not auto-index every TEXT
//! column.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

fn ids(result: uqa_sql::SQLResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .filter_map(|row| match row.get("id") {
            Some(Value::Int(id)) => Some(*id),
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
    assert_eq!(ids(content_hits), vec![1]);

    let context_hits = eng
        .sql(
            "SELECT id FROM messages WHERE text_match(context_json, '대구호텔') ORDER BY id",
            &[],
        )
        .unwrap();
    assert!(context_hits.rows.is_empty());

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
        assert_eq!(ids(hits), vec![1, 2]);

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
