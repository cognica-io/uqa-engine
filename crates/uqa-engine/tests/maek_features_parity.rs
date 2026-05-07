//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Maek-side feature parity test. Mirrors the SQL surface that
//! `maek/src/renderer/memory/MemoryStore.ts` drives the engine
//! through. If a Maek-required SQL feature regresses in uqa-rs,
//! this test fails first.
//!
//! Coverage:
//!   - DDL: CREATE TABLE with BIGSERIAL / TEXT NOT NULL DEFAULT /
//!     INTEGER / BIGINT / TEXT UNIQUE / VECTOR(N).
//!   - Index DDL: regular B-tree, GIN with `WITH (analyzer = ...)`,
//!     HNSW vector index.
//!   - Migration: ALTER TABLE ADD COLUMN with NOT NULL DEFAULT.
//!   - Idempotent housekeeping: DROP TABLE / INDEX IF EXISTS.
//!   - DML: parameterised INSERT / UPDATE / DELETE with $N
//!     placeholders, including LIKE on both parameterised and
//!     inline-literal patterns.
//!   - Hybrid retrieval: `fuse_log_odds(bayesian_match(...),
//!     knn_match(...))` with both inline-literal vectors
//!     (`ARRAY[...]`) and `$N` `SQLParam::vector` bindings.
//!   - Persistence: `Engine::open(path)` reopen survives schema
//!     and BIGSERIAL watermark.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

fn open_engine(dir: &TempDir) -> Engine {
    Engine::open(&dir.path().join("maek.db")).expect("open engine")
}

fn int_col(row: &uqa_sql::ResultRow, col: &str) -> Option<i64> {
    match row.get(col)? {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

fn str_col<'a>(row: &'a uqa_sql::ResultRow, col: &str) -> Option<&'a str> {
    match row.get(col)? {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

#[test]
fn root_tables_create_with_text_defaults_and_round_trip() {
    let dir = TempDir::new().unwrap();
    let eng = open_engine(&dir);
    eng.sql(
        "CREATE TABLE IF NOT EXISTS conversations (
           id TEXT PRIMARY KEY,
           shard_id BIGINT NOT NULL,
           title TEXT NOT NULL,
           provider TEXT NOT NULL,
           chat_model TEXT NOT NULL,
           embedding_provider TEXT NOT NULL,
           embedding_model TEXT NOT NULL,
           system_prompt TEXT NOT NULL DEFAULT '',
           embedding_dims INTEGER,
           retrieval_top_k INTEGER,
           retrieval_max_context_tokens INTEGER,
           enabled_tools TEXT NOT NULL DEFAULT '[]',
           created_at BIGINT NOT NULL,
           reasoning_effort TEXT NOT NULL DEFAULT 'medium'
         )",
        &[],
    )
    .expect("create conversations");
    eng.sql(
        "CREATE TABLE IF NOT EXISTS engine_meta (
           schema_version INTEGER NOT NULL DEFAULT 0
         )",
        &[],
    )
    .expect("create engine_meta");

    eng.sql(
        "INSERT INTO conversations
           (id, shard_id, title, provider, chat_model,
            embedding_provider, embedding_model, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[
            SQLParam::scalar(Value::Str("c-test".into())),
            SQLParam::scalar(Value::Int(1)),
            SQLParam::scalar(Value::Str("Hello".into())),
            SQLParam::scalar(Value::Str("openai".into())),
            SQLParam::scalar(Value::Str("gpt-x".into())),
            SQLParam::scalar(Value::Str("openai".into())),
            SQLParam::scalar(Value::Str("text-embedding-3-small".into())),
            SQLParam::scalar(Value::Int(1234)),
        ],
    )
    .expect("insert conversation");

    let r = eng
        .sql(
            "SELECT id, title, system_prompt, enabled_tools, reasoning_effort
             FROM conversations",
            &[],
        )
        .expect("select conversation");
    assert_eq!(r.rows.len(), 1);
    let row = &r.rows[0];
    assert_eq!(str_col(row, "id"), Some("c-test"));
    assert_eq!(str_col(row, "title"), Some("Hello"));
    assert_eq!(str_col(row, "system_prompt"), Some(""));
    assert_eq!(str_col(row, "enabled_tools"), Some("[]"));
    assert_eq!(str_col(row, "reasoning_effort"), Some("medium"));
}

#[test]
fn alter_table_add_column_with_not_null_default() {
    let dir = TempDir::new().unwrap();
    let eng = open_engine(&dir);
    eng.sql(
        "CREATE TABLE messages (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO messages (body) VALUES ('a')", &[])
        .unwrap();
    eng.sql(
        "ALTER TABLE messages ADD COLUMN kind TEXT NOT NULL DEFAULT 'chat'",
        &[],
    )
    .unwrap();
    let r = eng
        .sql("SELECT id, body, kind FROM messages", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(str_col(&r.rows[0], "kind"), Some("chat"));
}

#[test]
fn drop_table_and_drop_index_idempotent() {
    let dir = TempDir::new().unwrap();
    let eng = open_engine(&dir);
    eng.sql("DROP TABLE IF EXISTS missing_table", &[]).unwrap();
    eng.sql("DROP INDEX IF EXISTS missing_idx", &[]).unwrap();
    eng.sql(
        "CREATE TABLE foo (id BIGSERIAL PRIMARY KEY, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("CREATE INDEX foo_body_idx ON foo (body)", &[])
        .unwrap();
    eng.sql("DROP INDEX IF EXISTS foo_body_idx", &[]).unwrap();
    eng.sql("DROP TABLE IF EXISTS foo", &[]).unwrap();
}

#[test]
fn shard_table_creates_with_vector_column_and_three_index_kinds() {
    let dir = TempDir::new().unwrap();
    let eng = open_engine(&dir);
    eng.sql(
        "CREATE TABLE messages_c1 (
           id BIGSERIAL PRIMARY KEY,
           public_id TEXT UNIQUE,
           conversation_id TEXT NOT NULL,
           turn_index INTEGER NOT NULL,
           role TEXT NOT NULL,
           content TEXT NOT NULL,
           created_at BIGINT NOT NULL,
           token_count INTEGER NOT NULL,
           kind TEXT NOT NULL DEFAULT 'chat',
           embedding VECTOR(8)
         )",
        &[],
    )
    .expect("create messages_c1");
    eng.sql(
        "CREATE INDEX messages_c1_content_idx \
         ON messages_c1 USING gin (content) \
         WITH (analyzer = 'standard_cjk')",
        &[],
    )
    .expect("create gin index with cjk analyzer");
    eng.sql(
        "CREATE INDEX messages_c1_embedding_idx \
         ON messages_c1 USING hnsw (embedding)",
        &[],
    )
    .expect("create hnsw index");
    eng.sql(
        "CREATE INDEX messages_c1_public_id_idx \
         ON messages_c1 (public_id)",
        &[],
    )
    .expect("create btree index on public_id");

    for i in 0..3 {
        eng.sql(
            "INSERT INTO messages_c1
              (public_id, conversation_id, turn_index, role,
               content, created_at, token_count, embedding)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                SQLParam::scalar(Value::Str(format!("pub-{i}"))),
                SQLParam::scalar(Value::Str("c1".into())),
                SQLParam::scalar(Value::Int(i)),
                SQLParam::scalar(Value::Str("user".into())),
                SQLParam::scalar(Value::Str(format!("hello world {i}"))),
                SQLParam::scalar(Value::Int(1000 + i)),
                SQLParam::scalar(Value::Int(2)),
                SQLParam::vector(vec![
                    i as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                ]),
            ],
        )
        .unwrap_or_else(|e| panic!("insert {i}: {e:?}"));
    }
    let r = eng
        .sql("SELECT id FROM messages_c1", &[])
        .expect("select messages");
    assert_eq!(r.rows.len(), 3);
}

#[test]
fn parameterised_insert_select_update_delete() {
    let dir = TempDir::new().unwrap();
    let eng = open_engine(&dir);
    eng.sql(
        "CREATE TABLE c (id TEXT PRIMARY KEY, title TEXT NOT NULL)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO c (id, title) VALUES ($1, $2), ($3, $4)",
        &[
            SQLParam::scalar(Value::Str("a".into())),
            SQLParam::scalar(Value::Str("old".into())),
            SQLParam::scalar(Value::Str("b".into())),
            SQLParam::scalar(Value::Str("keep".into())),
        ],
    )
    .unwrap();
    let upd = eng
        .sql(
            "UPDATE c SET title = $1 WHERE id = $2",
            &[
                SQLParam::scalar(Value::Str("new".into())),
                SQLParam::scalar(Value::Str("a".into())),
            ],
        )
        .unwrap();
    assert_eq!(upd.affected_rows, 1);
    let del = eng
        .sql(
            "DELETE FROM c WHERE id = $1",
            &[SQLParam::scalar(Value::Str("b".into()))],
        )
        .unwrap();
    assert_eq!(del.affected_rows, 1);
    let r = eng.sql("SELECT id, title FROM c", &[]).unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(str_col(&r.rows[0], "id"), Some("a"));
    assert_eq!(str_col(&r.rows[0], "title"), Some("new"));
}

#[test]
fn like_predicate_parameterised_and_inline_paths_both_work() {
    let dir = TempDir::new().unwrap();
    let eng = open_engine(&dir);
    eng.sql(
        "CREATE TABLE m (id BIGSERIAL PRIMARY KEY, content TEXT NOT NULL)",
        &[],
    )
    .unwrap();
    let bodies = [
        "_Image prompt:_ red apple\n\n![alt](maek-image://image/c1/abc.png)",
        "Just a plain text reply",
        "_Video prompt:_ slow dolly\n\n![alt](maek-video://video/c1/abc.mp4)",
        "Plain mention of maek-image without scheme",
    ];
    for b in &bodies {
        eng.sql(
            "INSERT INTO m (content) VALUES ($1)",
            &[SQLParam::scalar(Value::Str((*b).into()))],
        )
        .unwrap();
    }
    let r = eng
        .sql(
            "SELECT id FROM m WHERE content LIKE $1 OR content LIKE $2",
            &[
                SQLParam::scalar(Value::Str("%maek-image://%".into())),
                SQLParam::scalar(Value::Str("%maek-video://%".into())),
            ],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2, "parameterised LIKE expected 2 hits");

    let r2 = eng
        .sql(
            "SELECT id FROM m \
             WHERE content LIKE '%maek-image://%' \
                OR content LIKE '%maek-video://%'",
            &[],
        )
        .unwrap();
    assert_eq!(r2.rows.len(), 2, "inline LIKE expected 2 hits");
}

#[test]
fn enabled_tools_json_text_round_trip() {
    let dir = TempDir::new().unwrap();
    let eng = open_engine(&dir);
    eng.sql(
        "CREATE TABLE c (
           id TEXT PRIMARY KEY,
           enabled_tools TEXT NOT NULL DEFAULT '[]'
         )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO c (id, enabled_tools) VALUES ($1, $2)",
        &[
            SQLParam::scalar(Value::Str("a".into())),
            SQLParam::scalar(Value::Str(
                r#"["web_search","wikipedia"]"#.into(),
            )),
        ],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT enabled_tools FROM c WHERE id = $1",
            &[SQLParam::scalar(Value::Str("a".into()))],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(
        str_col(&r.rows[0], "enabled_tools"),
        Some(r#"["web_search","wikipedia"]"#),
    );
}

#[test]
fn hybrid_retrieval_via_fuse_log_odds_inline_vector_literal() {
    let dir = TempDir::new().unwrap();
    let eng = open_engine(&dir);
    eng.sql(
        "CREATE TABLE notes (
           id BIGSERIAL PRIMARY KEY,
           content TEXT NOT NULL,
           embedding VECTOR(4)
         )",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX notes_content_idx \
         ON notes USING gin (content) \
         WITH (analyzer = 'standard_cjk')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX notes_embedding_idx \
         ON notes USING hnsw (embedding)",
        &[],
    )
    .unwrap();
    let docs = [
        ("hello world tabby cat sunshine", [0.9_f32, 0.1, 0.0, 0.0]),
        ("siamese cat vocal elegant", [0.1, 0.9, 0.0, 0.0]),
        ("ragdoll large gentle calm", [0.0, 0.1, 0.9, 0.0]),
    ];
    for (txt, vec) in &docs {
        eng.sql(
            "INSERT INTO notes (content, embedding) VALUES ($1, $2)",
            &[
                SQLParam::scalar(Value::Str((*txt).into())),
                SQLParam::vector(vec.to_vec()),
            ],
        )
        .unwrap();
    }
    let q = "SELECT id, _score FROM notes \
             WHERE fuse_log_odds( \
               bayesian_match(content, $1), \
               knn_match(embedding, ARRAY[0.9, 0.1, 0.0, 0.0], 3) \
             ) \
             ORDER BY _score DESC \
             LIMIT 3";
    let r = eng
        .sql(
            q,
            &[SQLParam::scalar(Value::Str("tabby cat".into()))],
        )
        .expect("fuse_log_odds query");
    assert!(!r.rows.is_empty(), "fuse_log_odds returned no rows");
    let top_id = int_col(&r.rows[0], "id");
    assert_eq!(top_id, Some(1), "top hit was not the tabby doc");
}

#[test]
fn hybrid_retrieval_via_fuse_log_odds_param_vector_binding() {
    let dir = TempDir::new().unwrap();
    let eng = open_engine(&dir);
    eng.sql(
        "CREATE TABLE notes (
           id BIGSERIAL PRIMARY KEY,
           content TEXT NOT NULL,
           embedding VECTOR(4)
         )",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX notes_content_idx \
         ON notes USING gin (content) \
         WITH (analyzer = 'standard_cjk')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX notes_embedding_idx \
         ON notes USING hnsw (embedding)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (content, embedding) VALUES \
           ('alpha apple aardvark', ARRAY[0.9, 0.1, 0.0, 0.0]), \
           ('beta banana baseball', ARRAY[0.1, 0.9, 0.0, 0.0]), \
           ('gamma grape glove',    ARRAY[0.0, 0.1, 0.9, 0.0])",
        &[],
    )
    .unwrap();
    let q = "SELECT id, _score FROM notes \
             WHERE fuse_log_odds( \
               bayesian_match(content, $1), \
               knn_match(embedding, $2, 3) \
             ) \
             ORDER BY _score DESC \
             LIMIT 3";
    let r = eng
        .sql(
            q,
            &[
                SQLParam::scalar(Value::Str("alpha apple".into())),
                SQLParam::vector(vec![0.9, 0.1, 0.0, 0.0]),
            ],
        )
        .expect("param-bound fuse_log_odds query");
    assert!(!r.rows.is_empty());
    let top_id = int_col(&r.rows[0], "id");
    assert_eq!(top_id, Some(1));
}

#[test]
fn schema_and_data_persist_across_engine_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("maek.db");
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            "CREATE TABLE messages_c1 (
               id BIGSERIAL PRIMARY KEY,
               public_id TEXT UNIQUE,
               content TEXT NOT NULL,
               embedding VECTOR(4)
             )",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO messages_c1 (public_id, content, embedding) \
             VALUES ($1, $2, $3)",
            &[
                SQLParam::scalar(Value::Str("p1".into())),
                SQLParam::scalar(Value::Str("hello".into())),
                SQLParam::vector(vec![0.1, 0.2, 0.3, 0.4]),
            ],
        )
        .unwrap();
    }
    {
        let eng = Engine::open(&path).unwrap();
        let r = eng
            .sql("SELECT public_id, content FROM messages_c1", &[])
            .unwrap();
        assert_eq!(r.rows.len(), 1);
        assert_eq!(str_col(&r.rows[0], "public_id"), Some("p1"));

        eng.sql(
            "INSERT INTO messages_c1 (public_id, content, embedding) \
             VALUES ($1, $2, $3)",
            &[
                SQLParam::scalar(Value::Str("p2".into())),
                SQLParam::scalar(Value::Str("world".into())),
                SQLParam::vector(vec![0.5, 0.6, 0.7, 0.8]),
            ],
        )
        .unwrap();
        let ids = eng
            .sql("SELECT id FROM messages_c1 ORDER BY id", &[])
            .unwrap();
        assert_eq!(ids.rows.len(), 2);
        let id1 = int_col(&ids.rows[0], "id").expect("id1 int");
        let id2 = int_col(&ids.rows[1], "id").expect("id2 int");
        assert!(
            id2 > id1,
            "BIGSERIAL watermark did not advance after reopen \
             (id1={id1}, id2={id2})",
        );
    }
}
