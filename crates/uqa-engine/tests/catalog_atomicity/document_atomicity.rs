//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn sql_autocommit_rolls_back_documents_text_and_vectors_together() {
    let (_dir, connection, engine) = persistent_engine();
    engine
        .sql(
            "CREATE TABLE atomic_docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX atomic_docs_body_gin ON atomic_docs USING gin (body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX atomic_docs_embedding_ivf ON atomic_docs USING ivf (embedding) WITH (lists = 1, probes = 1, train_threshold = 1)",
            &[],
        )
        .unwrap();

    // Vector persistence is the final store touched by this INSERT. Failing
    // there must undo the document and FTS writes that already succeeded.
    fail_event(&connection, "_vectors", "INSERT");
    assert!(engine
        .sql(
            "INSERT INTO atomic_docs VALUES (1, 'must roll back', ARRAY[1.0, 0.0])",
            &[],
        )
        .is_err());
    clear_failure(&connection);

    assert!(engine.get_document("atomic_docs", 1).unwrap().is_none());
    assert!(engine
        .search(
            "atomic_docs",
            "body",
            "roll",
            &uqa_engine::ScoringMode::default(),
            10,
        )
        .unwrap()
        .is_empty());
    assert!(engine
        .knn_search("atomic_docs", "embedding", [1.0, 0.0], 10)
        .unwrap()
        .is_empty());
}

#[test]
fn failed_commit_clears_engine_transaction_state_and_restores_caches() {
    let (_dir, connection, engine) = persistent_engine();
    engine
        .sql(
            "CREATE TABLE commit_t (id INTEGER PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();

    engine.begin().unwrap();
    engine
        .sql("INSERT INTO commit_t VALUES (1, 'uncommitted')", &[])
        .unwrap();
    let poisoned = connection.with(|conn| {
        conn.execute("INSERT INTO table_that_does_not_exist VALUES (1)", [])?;
        Ok(())
    });
    assert!(poisoned.is_err());
    assert!(engine.commit().is_err());

    assert_eq!(engine.transaction_depth(), 0);
    assert!(engine.get_document("commit_t", 1).unwrap().is_none());
    assert!(engine
        .sql("SELECT id FROM commit_t", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn direct_document_insert_rolls_back_text_and_vectors_together() {
    let (_dir, connection, engine) = persistent_engine();
    engine
        .sql(
            "CREATE TABLE direct_insert (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX direct_insert_body_gin ON direct_insert USING gin (body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX direct_insert_embedding_ivf ON direct_insert USING ivf (embedding) WITH (lists = 1, probes = 1, train_threshold = 1)",
            &[],
        )
        .unwrap();

    let mut document = Document::new();
    document.insert("id".into(), Value::Int(1));
    document.insert("body".into(), Value::Str("direct rollback".into()));
    let vectors = [("embedding".to_string(), vec![1.0, 0.0])]
        .into_iter()
        .collect();

    fail_event(&connection, "_vectors", "INSERT");
    assert!(engine
        .add_document_with_vectors("direct_insert", 1, document, vectors)
        .is_err());
    clear_failure(&connection);

    assert_eq!(engine.transaction_depth(), 0);
    assert!(engine.get_document("direct_insert", 1).unwrap().is_none());
    assert!(engine
        .search(
            "direct_insert",
            "body",
            "rollback",
            &uqa_engine::ScoringMode::default(),
            10,
        )
        .unwrap()
        .is_empty());
    assert!(engine
        .knn_search("direct_insert", "embedding", [1.0, 0.0], 10)
        .unwrap()
        .is_empty());
}

#[test]
fn direct_document_update_and_delete_restore_every_index_on_failure() {
    let (_dir, connection, engine) = persistent_engine();
    engine
        .sql(
            "CREATE TABLE direct_update (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX direct_update_body_gin ON direct_update USING gin (body)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX direct_update_embedding_ivf ON direct_update USING ivf (embedding) WITH (lists = 1, probes = 1, train_threshold = 1)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO direct_update VALUES (1, 'original token', ARRAY[1.0, 0.0])",
            &[],
        )
        .unwrap();

    let updates = [("body".to_string(), Value::Str("replacement token".into()))]
        .into_iter()
        .collect();
    let vectors = [("embedding".to_string(), vec![vec![0.0, 1.0]])]
        .into_iter()
        .collect();
    fail_event(&connection, "_vectors", "INSERT");
    assert!(engine
        .update_document_fields_with_vector_values("direct_update", 1, updates, vectors)
        .is_err());
    clear_failure(&connection);

    let restored = engine.get_document("direct_update", 1).unwrap().unwrap();
    assert_eq!(
        restored.get("body"),
        Some(&Value::Str("original token".into()))
    );
    assert_eq!(
        engine
            .search(
                "direct_update",
                "body",
                "original",
                &uqa_engine::ScoringMode::default(),
                10,
            )
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        engine
            .knn_search("direct_update", "embedding", [1.0, 0.0], 10)
            .unwrap()
            .len(),
        1
    );

    fail_event(&connection, "_vectors", "DELETE");
    assert!(engine.delete_document("direct_update", 1).is_err());
    clear_failure(&connection);
    assert!(engine.get_document("direct_update", 1).unwrap().is_some());
    assert_eq!(
        engine
            .search(
                "direct_update",
                "body",
                "original",
                &uqa_engine::ScoringMode::default(),
                10,
            )
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        engine
            .knn_search("direct_update", "embedding", [1.0, 0.0], 10)
            .unwrap()
            .len(),
        1
    );
}
