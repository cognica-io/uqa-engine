//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn sqlite_point_update_by_public_id_preserves_unmodified_vector_field() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("point-update.db");

    {
        let engine = Engine::open(&db_path).unwrap();
        engine
            .sql(
                "CREATE TABLE messages (\
                 id INTEGER PRIMARY KEY, \
                 public_id TEXT UNIQUE, \
                 content TEXT, \
                 token_count INTEGER, \
                 embedding VECTOR(2))",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX idx_messages_content ON messages USING gin (content)",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO messages (id, public_id, content, token_count, embedding) VALUES \
                 (1, 'm-1', 'old alpha', 2, ARRAY[1.0, 0.0]), \
                 (2, 'm-2', 'old beta', 3, ARRAY[0.0, 1.0])",
                &[],
            )
            .unwrap();

        let result = engine
            .sql(
                "UPDATE messages SET content = 'new beta', token_count = 8 \
                 WHERE public_id = 'm-2'",
                &[],
            )
            .unwrap();
        assert_eq!(result.affected_rows, 1);

        let row = engine
            .sql(
                "SELECT content, token_count, embedding FROM messages WHERE public_id = 'm-2'",
                &[],
            )
            .unwrap()
            .rows
            .remove(0);
        assert_eq!(row.get("content"), Some(&Value::Str("new beta".into())));
        assert_eq!(row.get("token_count"), Some(&Value::Int(8)));
        assert_eq!(
            row.get("embedding"),
            Some(&Value::List(vec![Value::Float(0.0), Value::Float(1.0)]))
        );

        let hits = engine
            .sql(
                "SELECT public_id FROM messages WHERE text_match(content, 'new')",
                &[],
            )
            .unwrap();
        assert_eq!(hits.rows.len(), 1);
        assert_eq!(
            hits.rows[0].get("public_id"),
            Some(&Value::Str("m-2".into()))
        );
    }

    let reopened = Engine::open(&db_path).unwrap();
    let row = reopened
        .sql(
            "SELECT content, token_count, embedding FROM messages WHERE public_id = 'm-2'",
            &[],
        )
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(row.get("content"), Some(&Value::Str("new beta".into())));
    assert_eq!(row.get("token_count"), Some(&Value::Int(8)));
    assert_eq!(
        row.get("embedding"),
        Some(&Value::List(vec![Value::Float(0.0), Value::Float(1.0)]))
    );
}

#[test]
fn sqlite_point_update_reports_zero_when_lookup_field_does_not_match() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("point-update-miss.db");
    let engine = Engine::open(&db_path).unwrap();
    engine
        .sql(
            "CREATE TABLE messages (id INTEGER PRIMARY KEY, public_id TEXT, content TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO messages (id, public_id, content) VALUES (1, 'm-1', 'old')",
            &[],
        )
        .unwrap();

    let result = engine
        .sql(
            "UPDATE messages SET content = 'new' WHERE public_id = 'missing'",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 0);

    let row = engine
        .sql("SELECT content FROM messages WHERE public_id = 'm-1'", &[])
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(row.get("content"), Some(&Value::Str("old".into())));
}
