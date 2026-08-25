//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retrieval predicates over inherited and partitioned physical relations.

use super::exec;
use uqa_core::Value;
use uqa_engine::Engine;

fn integer_column(engine: &Engine, sql: &str, column: &str) -> Vec<i64> {
    engine
        .sql(sql, &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| match row.get(column) {
            Some(Value::Int(value)) => *value,
            other => panic!("expected integer column `{column}`, got {other:?}"),
        })
        .collect()
}

fn create_gin(engine: &Engine, index: &str, table: &str) {
    exec(
        engine,
        &format!("CREATE INDEX {index} ON {table} USING gin (body)"),
    );
}

#[test]
fn inherited_retrieval_includes_each_physical_row_once_and_honors_only() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE search_root (id INTEGER, body TEXT, embedding VECTOR(2))",
    );
    exec(
        &engine,
        "CREATE TABLE search_left (left_value INTEGER) INHERITS (search_root)",
    );
    exec(
        &engine,
        "CREATE TABLE search_right (right_value INTEGER) INHERITS (search_root)",
    );
    exec(
        &engine,
        "CREATE TABLE search_leaf (leaf_value INTEGER) INHERITS (search_left, search_right)",
    );
    create_gin(&engine, "search_root_body_gin", "search_root");
    create_gin(&engine, "search_left_body_gin", "search_left");
    create_gin(&engine, "search_right_body_gin", "search_right");
    create_gin(&engine, "search_leaf_body_gin", "search_leaf");
    exec(
        &engine,
        "INSERT INTO search_root VALUES (1, 'needle root', ARRAY[1.0, 0.0])",
    );
    exec(
        &engine,
        "INSERT INTO search_leaf (id, body, embedding, left_value, right_value, leaf_value) VALUES (2, 'needle leaf', ARRAY[0.9, 0.1], 10, 20, 30)",
    );

    assert_eq!(
        integer_column(
            &engine,
            "SELECT id, _score FROM search_root WHERE text_match(body, 'needle') ORDER BY id",
            "id",
        ),
        vec![1, 2]
    );
    assert_eq!(
        integer_column(
            &engine,
            "SELECT id, _score FROM ONLY search_root WHERE text_match(body, 'needle') ORDER BY id",
            "id",
        ),
        vec![1]
    );
}

#[test]
fn partition_retrieval_keeps_table_local_doc_ids_and_global_knn_support() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE vector_parent (id INTEGER, body TEXT, embedding VECTOR(2)) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE vector_low PARTITION OF vector_parent FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE vector_high PARTITION OF vector_parent FOR VALUES FROM (10) TO (20)",
    );
    create_gin(&engine, "vector_parent_body_gin", "vector_parent");
    create_gin(&engine, "vector_low_body_gin", "vector_low");
    create_gin(&engine, "vector_high_body_gin", "vector_high");
    exec(
        &engine,
        "INSERT INTO vector_parent VALUES (1, 'needle low best', ARRAY[1.0, 0.0]), (2, 'needle low second', ARRAY[0.8, 0.2]), (11, 'needle high best', ARRAY[0.99, 0.01]), (12, 'needle high second', ARRAY[0.0, 1.0])",
    );
    exec(
        &engine,
        "CREATE TABLE vector_flat (id INTEGER, body TEXT, embedding VECTOR(2))",
    );
    exec(
        &engine,
        "INSERT INTO vector_flat VALUES (1, 'needle low best', ARRAY[1.0, 0.0]), (2, 'needle low second', ARRAY[0.8, 0.2]), (11, 'needle high best', ARRAY[0.99, 0.01]), (12, 'needle high second', ARRAY[0.0, 1.0])",
    );

    let rows = engine
        .sql(
            "SELECT id, _score FROM vector_parent WHERE knn_match(embedding, ARRAY[1.0, 0.0], 2) ORDER BY _score DESC, id",
            &[],
        )
        .unwrap();
    let flat_rows = engine
        .sql(
            "SELECT id, _score FROM vector_flat WHERE knn_match(embedding, ARRAY[1.0, 0.0], 2) ORDER BY _score DESC, id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.rows[0]["id"], Value::Int(1));
    assert_eq!(rows.rows[1]["id"], Value::Int(11));
    assert_eq!(rows.rows, flat_rows.rows);

    let calibrated = engine
        .sql(
            "SELECT id, _score FROM vector_parent WHERE calibrated_vector_match('embedding', ARRAY[1.0, 0.0], 2) ORDER BY _score DESC, id",
            &[],
        )
        .unwrap();
    let flat_calibrated = engine
        .sql(
            "SELECT id, _score FROM vector_flat WHERE calibrated_vector_match('embedding', ARRAY[1.0, 0.0], 2) ORDER BY _score DESC, id",
            &[],
        )
        .unwrap();
    assert_eq!(calibrated.rows.len(), 2);
    assert_eq!(calibrated.rows[0]["id"], Value::Int(1));
    assert_eq!(calibrated.rows[1]["id"], Value::Int(11));
    assert_eq!(calibrated.rows, flat_calibrated.rows);
    assert!(engine
        .sql(
            "SELECT id FROM vector_parent WHERE calibrated_vector_match('embedding', ARRAY[1.0, 0.0], 2, 1.0)",
            &[],
        )
        .unwrap()
        .rows
        .is_empty());

    assert_eq!(
        integer_column(
            &engine,
            "SELECT id, _score FROM vector_parent WHERE text_match(body, 'needle') ORDER BY _score DESC, id LIMIT 2",
            "id",
        )
        .len(),
        2
    );
    assert!(engine
        .sql(
            "SELECT id FROM ONLY vector_parent WHERE knn_match(embedding, ARRAY[1.0, 0.0], 2)",
            &[],
        )
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn partition_hybrid_retrieval_ranks_descendant_scores_before_sql_limit() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE hybrid_parent (id INTEGER, body TEXT, embedding VECTOR(2)) PARTITION BY LIST (id)",
    );
    exec(
        &engine,
        "CREATE TABLE hybrid_first PARTITION OF hybrid_parent FOR VALUES IN (1, 2)",
    );
    exec(
        &engine,
        "CREATE TABLE hybrid_second PARTITION OF hybrid_parent FOR VALUES IN (3, 4)",
    );
    create_gin(&engine, "hybrid_parent_body_gin", "hybrid_parent");
    create_gin(&engine, "hybrid_first_body_gin", "hybrid_first");
    create_gin(&engine, "hybrid_second_body_gin", "hybrid_second");
    exec(
        &engine,
        "INSERT INTO hybrid_parent VALUES (1, 'needle first', ARRAY[1.0, 0.0]), (2, 'other first', ARRAY[0.8, 0.2]), (3, 'needle second', ARRAY[0.9, 0.1]), (4, 'other second', ARRAY[0.0, 1.0])",
    );

    let rows = engine
        .sql(
            "SELECT id, _score FROM hybrid_parent WHERE text_match(body, 'needle') AND knn_match(embedding, ARRAY[1.0, 0.0], 4) ORDER BY _score DESC, id LIMIT 2",
            &[],
        )
        .unwrap();
    let ids = rows
        .rows
        .iter()
        .map(|row| row["id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&Value::Int(1)));
    assert!(ids.contains(&Value::Int(3)));
}

#[test]
fn retrieval_row_locks_keep_the_physical_table_identity() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("hierarchy-retrieval-lock.db")).unwrap();
    exec(
        &engine,
        "CREATE TABLE lock_search_parent (id INTEGER, body TEXT) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE lock_search_low PARTITION OF lock_search_parent FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE lock_search_high PARTITION OF lock_search_parent FOR VALUES FROM (10) TO (20)",
    );
    create_gin(&engine, "lock_search_parent_body_gin", "lock_search_parent");
    create_gin(&engine, "lock_search_low_body_gin", "lock_search_low");
    create_gin(&engine, "lock_search_high_body_gin", "lock_search_high");
    exec(
        &engine,
        "INSERT INTO lock_search_parent VALUES (1, 'needle low'), (11, 'needle high')",
    );
    let holder = engine.new_session().unwrap();
    let waiter = engine.new_session().unwrap();

    exec(&holder, "BEGIN");
    holder
        .sql(
            "SELECT id FROM lock_search_parent WHERE text_match(body, 'needle') AND id = 11 FOR UPDATE",
            &[],
        )
        .unwrap();
    waiter
        .sql(
            "SELECT id FROM lock_search_low WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap();
    let error = waiter
        .sql(
            "SELECT id FROM lock_search_high WHERE id = 11 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55P03"));
    exec(&holder, "ROLLBACK");
}
