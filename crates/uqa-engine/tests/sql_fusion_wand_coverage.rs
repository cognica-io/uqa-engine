//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL coverage for `test_fusion_wand`.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLParam;

fn engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id SERIAL PRIMARY KEY, content TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX idx_docs_gin ON docs USING gin (content)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (content) VALUES \
             ('machine learning algorithms'), \
             ('deep learning neural networks'), \
             ('database indexing structures')",
            &[],
        )
        .unwrap();
    engine
}

fn hybrid_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE messages (\
             id SERIAL PRIMARY KEY, \
             content TEXT, \
             kind TEXT NOT NULL DEFAULT 'chat', \
             embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_messages_gin ON messages USING gin (content)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO messages (content, kind, embedding) VALUES \
             ('machine learning algorithms', 'chat', ARRAY[0.9, 0.1]), \
             ('deep learning neural networks', 'image', ARRAY[0.8, 0.2]), \
             ('database indexing structures', 'chat', ARRAY[0.1, 0.9])",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn test_log_odds_fusion_with_limit() {
    let result = engine()
        .sql(
            "SELECT * FROM docs WHERE \
             fuse_log_odds(bayesian_match(content, 'learning'), \
             bayesian_match(content, 'algorithms')) LIMIT 1",
            &[],
        )
        .unwrap();
    assert!(result.rows.len() <= 1);
}

#[test]
fn test_fusion_result_scores() {
    let result = engine()
        .sql(
            "SELECT content, _score FROM docs WHERE \
             fuse_log_odds(bayesian_match(content, 'learning'), \
             bayesian_match(content, 'algorithms'))",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
    for row in result.rows {
        match row.get("_score") {
            Some(Value::Float(score)) => assert!(*score > 0.0 && *score < 1.0),
            other => panic!("missing _score: {other:?}"),
        }
    }
}

#[test]
fn test_log_odds_fusion_with_default_alpha_and_filter() {
    let engine = hybrid_engine();

    let result = engine
        .sql(
            "SELECT kind, _score FROM messages WHERE \
             fuse_log_odds(\
                 bayesian_match(content, 'learning'), \
                 knn_match(embedding, ARRAY[0.9, 0.1], 3)\
             ) AND kind = 'chat' \
             ORDER BY _score DESC \
             LIMIT 3",
            &[],
        )
        .unwrap();

    assert!(!result.rows.is_empty());
    for row in result.rows {
        assert_eq!(row.get("kind"), Some(&Value::Str("chat".into())));
        match row.get("_score") {
            Some(Value::Float(score)) => assert!(*score > 0.0 && *score < 1.0),
            other => panic!("missing _score: {other:?}"),
        }
    }
}

#[test]
fn test_log_odds_fusion_inside_derived_table() {
    let result = hybrid_engine()
        .sql(
            "SELECT id, _score FROM (\
               SELECT id, _score FROM messages WHERE \
               fuse_log_odds(\
                   bayesian_match(content, 'learning'), \
                   knn_match(embedding, ARRAY[0.9, 0.1], 3)\
               ) AND kind = 'chat'\
             ) hits \
             ORDER BY _score DESC \
             LIMIT 2",
            &[],
        )
        .unwrap();

    assert!(!result.rows.is_empty());
    for row in result.rows {
        match row.get("_score") {
            Some(Value::Float(score)) => assert!(*score > 0.0 && *score < 1.0),
            other => panic!("missing _score: {other:?}"),
        }
    }
}

#[test]
fn test_log_odds_fusion_inside_union_branch() {
    let result = hybrid_engine()
        .sql(
            "SELECT source, id, _score FROM (\
               SELECT 'a' AS source, id, _score FROM messages WHERE \
               fuse_log_odds(\
                   bayesian_match(content, 'learning'), \
                   knn_match(embedding, ARRAY[0.9, 0.1], 3)\
               ) AND kind = 'chat' \
               UNION ALL \
               SELECT 'b' AS source, id, _score FROM messages WHERE \
               fuse_log_odds(\
                   bayesian_match(content, 'indexing'), \
                   knn_match(embedding, ARRAY[0.1, 0.9], 3)\
               ) AND kind = 'chat'\
             ) hits \
             ORDER BY _score DESC \
             LIMIT 4",
            &[],
        )
        .unwrap();

    assert!(!result.rows.is_empty());
    let sources: Vec<_> = result
        .rows
        .iter()
        .filter_map(|row| row.get("source"))
        .collect();
    assert!(sources.contains(&&Value::Str("a".into())));
    assert!(sources.contains(&&Value::Str("b".into())));
}

#[test]
fn test_log_odds_fusion_preserves_parameter_projection_inside_union_branch() {
    let result = hybrid_engine()
        .sql(
            "SELECT source, id, _score FROM (\
               SELECT $1 AS source, id, _score FROM messages WHERE \
               fuse_log_odds(\
                   bayesian_match(content, 'learning'), \
                   knn_match(embedding, ARRAY[0.9, 0.1], 3)\
               ) AND kind = 'chat' \
               UNION ALL \
               SELECT $2 AS source, id, _score FROM messages WHERE \
               fuse_log_odds(\
                   bayesian_match(content, 'indexing'), \
                   knn_match(embedding, ARRAY[0.1, 0.9], 3)\
               ) AND kind = 'chat'\
             ) hits \
             ORDER BY _score DESC \
             LIMIT 4",
            &[
                SQLParam::scalar(Value::Str("a".into())),
                SQLParam::scalar(Value::Str("b".into())),
            ],
        )
        .unwrap();

    assert!(!result.rows.is_empty());
    let sources: Vec<_> = result
        .rows
        .iter()
        .filter_map(|row| row.get("source"))
        .collect();
    assert!(sources.contains(&&Value::Str("a".into())));
    assert!(sources.contains(&&Value::Str("b".into())));
}

#[test]
fn test_log_odds_fusion_preserves_every_multi_union_batch_branch() {
    let result = hybrid_engine()
        .sql(
            "SELECT 0 AS query_index, id, _score FROM (\
               SELECT id, _score FROM messages WHERE \
               fuse_log_odds(\
                   bayesian_match(content, $1), \
                   knn_match(embedding, $2, 3)\
               ) AND kind = 'chat' ORDER BY _score DESC LIMIT 1\
             ) q0 \
             UNION ALL \
             SELECT 1 AS query_index, id, _score FROM (\
               SELECT id, _score FROM messages WHERE \
               fuse_log_odds(\
                   bayesian_match(content, $3), \
                   knn_match(embedding, $4, 3)\
               ) AND kind = 'chat' ORDER BY _score DESC LIMIT 1\
             ) q1 \
             UNION ALL \
             SELECT 2 AS query_index, id, _score FROM (\
               SELECT id, _score FROM messages WHERE \
               fuse_log_odds(\
                   bayesian_match(content, $5), \
                   knn_match(embedding, $6, 3)\
               ) AND kind = 'chat' ORDER BY _score DESC LIMIT 1\
             ) q2 \
             UNION ALL \
             SELECT 3 AS query_index, id, _score FROM (\
               SELECT id, _score FROM messages WHERE \
               fuse_log_odds(\
                   bayesian_match(content, $7), \
                   knn_match(embedding, $8, 3)\
               ) AND kind = 'chat' ORDER BY _score DESC LIMIT 1\
             ) q3",
            &[
                SQLParam::scalar(Value::Str("learning".into())),
                SQLParam::vector(vec![0.9, 0.1]),
                SQLParam::scalar(Value::Str("indexing".into())),
                SQLParam::vector(vec![0.1, 0.9]),
                SQLParam::scalar(Value::Str("learning".into())),
                SQLParam::vector(vec![0.9, 0.1]),
                SQLParam::scalar(Value::Str("indexing".into())),
                SQLParam::vector(vec![0.1, 0.9]),
            ],
        )
        .unwrap();

    let mut query_indices = result
        .rows
        .iter()
        .map(|row| match row.get("query_index") {
            Some(Value::Int(value)) => *value,
            other => panic!("unexpected query_index: {other:?}"),
        })
        .collect::<Vec<_>>();
    query_indices.sort_unstable();
    assert_eq!(query_indices, vec![0, 1, 2, 3]);
}

#[test]
fn test_log_odds_fusion_inside_join_filter() {
    let engine = setup_log_odds_join_filter_engine();

    let single_table = engine
        .sql(
            "SELECT doc_id, _score FROM doc_chunks \
             WHERE fuse_log_odds(\
                 bayesian_match(content, 'learning'), \
                 knn_match(embedding, ARRAY[0.9, 0.1], 3)\
             ) \
             ORDER BY _score DESC \
             LIMIT 3",
            &[],
        )
        .unwrap();
    assert!(!single_table.rows.is_empty());

    let plain_join = engine
        .sql(
            "SELECT c.doc_id AS doc_id, d.public_id AS public_id \
             FROM doc_chunks c \
             JOIN docs d ON d.public_id = c.doc_id \
             ORDER BY c.doc_id",
            &[],
        )
        .unwrap();
    assert_eq!(plain_join.rows.len(), 2);

    let derived_join = engine
        .sql(
            "SELECT hits.doc_id AS doc_id, d.public_id AS public_id, hits._score AS _score \
             FROM (\
               SELECT doc_id, _score FROM doc_chunks \
               WHERE fuse_log_odds(\
                   bayesian_match(content, 'learning'), \
                   knn_match(embedding, ARRAY[0.9, 0.1], 3)\
               )\
             ) hits \
             JOIN docs d ON d.public_id = hits.doc_id \
             ORDER BY hits._score DESC \
             LIMIT 3",
            &[],
        )
        .unwrap();
    assert!(!derived_join.rows.is_empty());

    let result = engine
        .sql(
            "SELECT d.attached_message_id AS attached_message_id, _score \
             FROM doc_chunks c \
             JOIN docs d ON d.public_id = c.doc_id \
             WHERE fuse_log_odds(\
                 bayesian_match(c.content, 'learning'), \
                 knn_match(c.embedding, ARRAY[0.9, 0.1], 3)\
             ) \
             ORDER BY _score DESC \
             LIMIT 3",
            &[],
        )
        .unwrap();

    assert!(!result.rows.is_empty());
    assert_eq!(
        result.rows[0].get("attached_message_id"),
        Some(&Value::Str("msg-1".into()))
    );
    match result.rows[0].get("_score") {
        Some(Value::Float(score)) => assert!(*score > 0.0 && *score < 1.0),
        other => panic!("missing _score: {other:?}"),
    }
}

fn setup_log_odds_join_filter_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (\
             public_id TEXT PRIMARY KEY, \
             attached_message_id TEXT NOT NULL)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE doc_chunks (\
             id SERIAL PRIMARY KEY, \
             doc_id TEXT NOT NULL, \
             content TEXT, \
             embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_doc_chunks_gin ON doc_chunks USING gin (content)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (public_id, attached_message_id) VALUES \
             ('doc-1', 'msg-1'), \
             ('doc-2', 'msg-2')",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO doc_chunks (doc_id, content, embedding) VALUES \
             ('doc-1', 'machine learning notes', ARRAY[0.9, 0.1]), \
             ('doc-2', 'database indexing notes', ARRAY[0.1, 0.9])",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn test_log_odds_with_gating_relu() {
    let result = engine()
        .sql(
            "SELECT * FROM docs WHERE \
             fuse_log_odds(bayesian_match(content, 'learning'), \
             bayesian_match(content, 'algorithms'), 0.5, 'relu')",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[test]
fn test_log_odds_with_gating_swish() {
    let result = engine()
        .sql(
            "SELECT * FROM docs WHERE \
             fuse_log_odds(bayesian_match(content, 'learning'), \
             bayesian_match(content, 'algorithms'), 'swish')",
            &[],
        )
        .unwrap();
    assert!(!result.rows.is_empty());
}
