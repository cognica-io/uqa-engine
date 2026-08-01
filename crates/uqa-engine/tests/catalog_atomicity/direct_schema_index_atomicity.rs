//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn direct_column_drop_restores_schema_and_rows_on_catalog_failure() {
    let (_dir, connection, engine) = persistent_engine();
    engine
        .sql(
            "CREATE TABLE direct_schema (id INTEGER PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO direct_schema VALUES (1, 'preserved')", &[])
        .unwrap();

    fail_event(&connection, "_tables", "INSERT");
    assert!(engine.drop_column("direct_schema", "body").is_err());
    clear_failure(&connection);

    let columns = engine.describe_table("direct_schema").unwrap().unwrap();
    assert!(columns.iter().any(|column| column.name == "body"));
    assert_eq!(
        engine
            .get_document("direct_schema", 1)
            .unwrap()
            .unwrap()
            .get("body"),
        Some(&Value::Str("preserved".into()))
    );
}

#[test]
fn direct_constraint_replacement_publishes_only_after_catalog_success() {
    let (dir, connection, engine) = persistent_engine();
    engine
        .sql("CREATE TABLE constrained (id INTEGER)", &[])
        .unwrap();
    let constraint = uqa_sql::ast::TableKeyConstraint {
        name: Some("constrained_id_key".to_string()),
        kind: uqa_sql::ast::TableKeyConstraintKind::Unique,
        columns: vec!["id".to_string()],
        nulls_not_distinct: false,
    };

    fail_event(&connection, "_tables", "INSERT");
    assert!(engine
        .register_table_constraints("constrained", vec![], vec![], vec![constraint])
        .is_err());
    assert!(engine.key_constraints("constrained").unwrap().is_empty());
    clear_failure(&connection);
    drop(engine);
    drop(connection);

    let reopened = Engine::open(&dir.path().join("catalog.db")).unwrap();
    assert!(reopened.key_constraints("constrained").unwrap().is_empty());
}

#[test]
fn direct_catalog_index_failure_restores_registry_and_derived_index_policy() {
    let (dir, connection, engine) = persistent_engine();
    engine
        .sql("CREATE TABLE indexed (id INTEGER, value TEXT)", &[])
        .unwrap();

    fail_event(&connection, "_catalog_indexes", "INSERT");
    assert!(engine
        .register_catalog_index(
            "indexed_value_idx",
            "btree",
            "indexed",
            &["value".to_string()],
            &[],
        )
        .is_err());
    assert!(!engine.has_catalog_index("indexed_value_idx").unwrap());
    clear_failure(&connection);

    engine
        .register_catalog_index(
            "indexed_value_idx",
            "btree",
            "indexed",
            &["value".to_string()],
            &[],
        )
        .unwrap();
    fail_event(&connection, "_catalog_indexes", "DELETE");
    assert!(engine.drop_catalog_index("indexed_value_idx").is_err());
    assert!(engine.has_catalog_index("indexed_value_idx").unwrap());
    clear_failure(&connection);
    drop(engine);
    drop(connection);

    let reopened = Engine::open(&dir.path().join("catalog.db")).unwrap();
    assert!(reopened.has_catalog_index("indexed_value_idx").unwrap());
}

#[test]
fn replacing_a_catalog_index_removes_the_previous_btree_policy() {
    let (_dir, connection, engine) = persistent_engine();
    engine
        .sql("CREATE TABLE old_table (value TEXT)", &[])
        .unwrap();
    engine
        .sql("CREATE TABLE new_table (value TEXT)", &[])
        .unwrap();
    engine
        .register_catalog_index(
            "moving_idx",
            "btree",
            "old_table",
            &["value".to_string()],
            &[],
        )
        .unwrap();
    connection
        .with(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _btree_indexes WHERE table_name = 'public.old_table' AND field = 'value'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1);
            Ok(())
        })
        .unwrap();

    engine
        .register_catalog_index(
            "moving_idx",
            "gin",
            "new_table",
            &["value".to_string()],
            &[],
        )
        .unwrap();
    connection
        .with(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _btree_indexes WHERE table_name = 'public.old_table' AND field = 'value'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn direct_alter_sequence_failure_preserves_state_through_reopen() {
    let (dir, connection, engine) = persistent_engine();
    engine.create_sequence("kept", 10, 2, false).unwrap();
    let before = engine.sequence_state("kept").unwrap().unwrap().1;

    fail_event(&connection, "_sequences", "UPDATE");
    assert!(engine
        .alter_sequence("kept", Some(Some(50)), Some(5), Some(20))
        .is_err());
    let after = engine.sequence_state("kept").unwrap().unwrap().1;
    assert_eq!(after.start, before.start);
    assert_eq!(after.increment, before.increment);
    assert_eq!(after.current, before.current);
    clear_failure(&connection);
    drop(engine);
    drop(connection);

    let reopened = Engine::open(&dir.path().join("catalog.db")).unwrap();
    let restored = reopened.sequence_state("kept").unwrap().unwrap().1;
    assert_eq!(restored.start, before.start);
    assert_eq!(restored.increment, before.increment);
    assert_eq!(restored.current, before.current);
}

#[test]
fn memory_direct_failure_preserves_detached_vectors_and_existing_rows() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE memory_atomic (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX memory_atomic_body_gin ON memory_atomic USING gin (body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX memory_atomic_embedding_ivf ON memory_atomic USING ivf (embedding) WITH (lists = 1, probes = 1, train_threshold = 1)",
            &[],
        )
        .unwrap();

    let mut original = Document::new();
    original.insert("body".into(), Value::Str("kept text".into()));
    let original_vectors = [("embedding".to_string(), vec![1.0, 0.0])]
        .into_iter()
        .collect();
    engine
        .add_document_with_vectors("memory_atomic", 1, original, original_vectors)
        .unwrap();

    let mut rejected = Document::new();
    rejected.insert("body".into(), Value::Str("must disappear".into()));
    let invalid_vectors = [("embedding".to_string(), vec![1.0, 0.0, 0.0])]
        .into_iter()
        .collect();
    assert!(engine
        .add_document_with_vectors("memory_atomic", 2, rejected, invalid_vectors)
        .is_err());

    assert!(engine.get_document("memory_atomic", 2).unwrap().is_none());
    assert_eq!(
        engine
            .knn_search("memory_atomic", "embedding", [1.0, 0.0], 10)
            .unwrap()
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(
        engine
            .search(
                "memory_atomic",
                "body",
                "kept",
                &uqa_engine::ScoringMode::default(),
                10,
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn memory_explicit_rollback_restores_schema_and_derived_indexes() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE memory_schema (id INTEGER PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX memory_schema_body_gin ON memory_schema USING gin (body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO memory_schema VALUES (1, 'rollback token')",
            &[],
        )
        .unwrap();

    engine.begin().unwrap();
    assert!(engine.drop_column("memory_schema", "body").unwrap());
    engine.rollback().unwrap();

    assert!(engine
        .describe_table("memory_schema")
        .unwrap()
        .unwrap()
        .iter()
        .any(|column| column.name == "body"));
    assert_eq!(
        engine
            .search(
                "memory_schema",
                "body",
                "rollback",
                &uqa_engine::ScoringMode::default(),
                10,
            )
            .unwrap()
            .len(),
        1
    );
}
