//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn memory_vector_validation_happens_before_document_publish() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX docs_embedding_idx ON docs USING hnsw (embedding)",
            &[],
        )
        .unwrap();
    let document = [
        ("id".to_string(), Value::Int(1)),
        ("body".to_string(), Value::Str("must not survive".into())),
    ]
    .into_iter()
    .collect();
    let vectors = [("embedding".to_string(), vec![f32::NAN, 0.0])]
        .into_iter()
        .collect();

    assert!(engine
        .add_document_with_vectors("docs", 1, document, vectors)
        .is_err());
    assert!(engine.get_document("docs", 1).unwrap().is_none());
    assert_eq!(engine.document_count("docs").unwrap(), 0);
}

#[test]
fn registry_writes_fail_before_memory_is_published() {
    let (dir, connection, engine) = persistent_engine();

    fail_event(&connection, "_schemas", "INSERT");
    assert!(engine.register_schema("broken", false).is_err());
    assert!(!engine.has_schema("broken").unwrap());
    clear_failure(&connection);

    fail_event(&connection, "_tables", "INSERT");
    assert!(engine.create_default_table("broken_table", vec![]).is_err());
    assert!(!engine.has_table("broken_table").unwrap());
    clear_failure(&connection);

    fail_event(&connection, "_analyzers", "INSERT");
    assert!(engine
        .register_named_analyzer(
            "broken_analyzer",
            r#"{"tokenizer":{"type":"standard"},"token_filters":[],"char_filters":[]}"#,
        )
        .is_err());
    assert!(!engine
        .list_named_analyzers()
        .unwrap()
        .iter()
        .any(|name| name == "broken_analyzer"));
    clear_failure(&connection);

    fail_event(&connection, "_foreign_servers", "INSERT");
    assert!(engine
        .register_foreign_server("broken_server".into(), "memory_fdw".into(), vec![], false,)
        .is_err());
    assert!(engine.foreign_server("broken_server").unwrap().is_none());
    clear_failure(&connection);

    fail_event(&connection, "_sequences", "INSERT");
    assert!(engine.create_sequence("broken_seq", 1, 1, false).is_err());
    assert!(engine.sequence_state("broken_seq").unwrap().is_none());
    clear_failure(&connection);

    fail_event(&connection, "_views", "INSERT");
    assert!(engine
        .sql("CREATE VIEW broken_view AS SELECT 1", &[])
        .is_err());
    assert!(engine.view("broken_view").unwrap().is_none());
    clear_failure(&connection);

    fail_event(&connection, "_metadata", "INSERT");
    assert!(engine
        .sql(
            "CREATE FUNCTION broken_fn() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'",
            &[],
        )
        .is_err());
    assert!(engine.sql("SELECT broken_fn()", &[]).is_err());
    clear_failure(&connection);

    fail_event(&connection, "_named_graphs", "INSERT");
    assert!(engine.create_graph("broken_graph").is_err());
    assert!(!engine.has_graph("broken_graph").unwrap());
    clear_failure(&connection);

    fail_event(&connection, "_models", "INSERT");
    let model = DeepModel {
        layers: vec![DeepLayerSpec::Embed {
            embedding: vec![1.0],
        }],
        alpha: 0.0,
        gating: GatingSpec::None,
    };
    assert!(engine.save_model("broken_model", &model).is_err());
    assert!(engine.load_model("broken_model").unwrap().is_none());
    clear_failure(&connection);

    fail_event(&connection, "_scoring_params", "INSERT");
    assert!(engine.save_scoring_params("broken_params", "{}").is_err());
    assert!(engine
        .load_scoring_params("broken_params")
        .unwrap()
        .is_none());
    clear_failure(&connection);

    drop(engine);
    drop(connection);
    let reopened = Engine::open(&dir.path().join("catalog.db")).unwrap();
    assert!(!reopened.has_schema("broken").unwrap());
    assert!(!reopened.has_table("broken_table").unwrap());
    assert!(!reopened
        .list_named_analyzers()
        .unwrap()
        .iter()
        .any(|name| name == "broken_analyzer"));
    assert!(reopened.sequence_state("broken_seq").unwrap().is_none());
    assert!(reopened.load_model("broken_model").unwrap().is_none());
    assert!(reopened
        .load_scoring_params("broken_params")
        .unwrap()
        .is_none());
}

fn verify_schema_and_analyzer_delete_atomicity(connection: &ManagedConnection, engine: &Engine) {
    engine.register_schema("kept_schema", false).unwrap();
    fail_event(connection, "_schemas", "DELETE");
    assert!(engine.drop_schema("kept_schema").is_err());
    assert!(engine.has_schema("kept_schema").unwrap());
    clear_failure(connection);

    let analyzer_json = r#"{"tokenizer":{"type":"standard"},"token_filters":[],"char_filters":[]}"#;
    engine
        .register_named_analyzer("kept_analyzer", analyzer_json)
        .unwrap();
    fail_event(connection, "_analyzers", "DELETE");
    assert!(engine.drop_named_analyzer("kept_analyzer").is_err());
    assert!(engine
        .list_named_analyzers()
        .unwrap()
        .iter()
        .any(|name| name == "kept_analyzer"));
    clear_failure(connection);

    engine
        .sql("CREATE TABLE analyzed (body TEXT)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE INDEX analyzed_body_gin ON analyzed USING gin (body)",
            &[],
        )
        .unwrap();
    engine
        .register_named_analyzer("replacement_analyzer", analyzer_json)
        .unwrap();
    engine
        .set_table_field_analyzer("analyzed", "body", "kept_analyzer", "both")
        .unwrap();
    fail_event(connection, "_table_field_analyzers", "INSERT");
    assert!(engine
        .set_table_field_analyzer("analyzed", "body", "replacement_analyzer", "both")
        .is_err());
    assert_eq!(
        engine.table_field_analyzer("analyzed", "body").unwrap(),
        Some(("kept_analyzer".into(), "both".into()))
    );
    clear_failure(connection);
}

fn verify_fdw_and_sequence_delete_atomicity(connection: &ManagedConnection, engine: &Engine) {
    engine
        .register_foreign_server("kept_server".into(), "memory_fdw".into(), vec![], false)
        .unwrap();
    fail_event(connection, "_foreign_servers", "DELETE");
    assert!(engine.drop_foreign_server("kept_server").is_err());
    assert!(engine.foreign_server("kept_server").unwrap().is_some());
    clear_failure(connection);

    engine
        .register_foreign_table(
            "kept_foreign".into(),
            "kept_server".into(),
            vec![],
            vec![],
            false,
        )
        .unwrap();
    fail_event(connection, "_foreign_tables", "DELETE");
    assert!(engine.drop_foreign_table("kept_foreign").is_err());
    assert!(engine.foreign_table("kept_foreign").unwrap().is_some());
    clear_failure(connection);

    engine.create_sequence("kept_seq", 1, 1, false).unwrap();
    let before = engine.sequence_state("kept_seq").unwrap().unwrap().1;
    fail_event(connection, "_sequences", "UPDATE");
    assert!(engine.nextval("kept_seq").is_err());
    assert_eq!(
        engine
            .sequence_state("kept_seq")
            .unwrap()
            .unwrap()
            .1
            .current,
        before.current
    );
    clear_failure(connection);

    fail_event(connection, "_sequences", "DELETE");
    assert!(engine.drop_sequence("kept_seq").is_err());
    assert!(engine.sequence_state("kept_seq").unwrap().is_some());
    clear_failure(connection);
}

fn verify_metadata_and_model_delete_atomicity(connection: &ManagedConnection, engine: &Engine) {
    engine
        .sql("CREATE VIEW kept_view AS SELECT 1", &[])
        .unwrap();
    fail_event(connection, "_views", "DELETE");
    assert!(engine.sql("DROP VIEW kept_view", &[]).is_err());
    assert!(engine.view("kept_view").unwrap().is_some());
    clear_failure(connection);

    engine
        .sql(
            "CREATE FUNCTION kept_fn() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'",
            &[],
        )
        .unwrap();
    fail_event(connection, "_metadata", "INSERT");
    assert!(engine.sql("DROP FUNCTION kept_fn()", &[]).is_err());
    clear_failure(connection);
    assert_eq!(
        engine.sql("SELECT kept_fn() AS value", &[]).unwrap().rows[0].get("value"),
        Some(&uqa_core::Value::Int(1))
    );

    let model = DeepModel {
        layers: vec![DeepLayerSpec::Embed {
            embedding: vec![1.0],
        }],
        alpha: 0.0,
        gating: GatingSpec::None,
    };
    engine.save_model("kept_model", &model).unwrap();
    fail_event(connection, "_models", "DELETE");
    assert!(engine.drop_model("kept_model").is_err());
    assert!(engine.load_model("kept_model").unwrap().is_some());
    clear_failure(connection);

    engine.save_scoring_params("kept_params", "{}").unwrap();
    fail_event(connection, "_scoring_params", "DELETE");
    assert!(engine.drop_scoring_params("kept_params").is_err());
    assert!(engine.load_scoring_params("kept_params").unwrap().is_some());
    clear_failure(connection);
}

#[test]
fn registry_deletes_and_updates_fail_before_memory_is_removed() {
    let (dir, connection, engine) = persistent_engine();
    verify_schema_and_analyzer_delete_atomicity(&connection, &engine);
    verify_fdw_and_sequence_delete_atomicity(&connection, &engine);
    verify_metadata_and_model_delete_atomicity(&connection, &engine);

    drop(engine);
    drop(connection);
    let reopened = Engine::open(&dir.path().join("catalog.db")).unwrap();
    assert!(reopened.has_schema("kept_schema").unwrap());
    assert!(reopened
        .list_named_analyzers()
        .unwrap()
        .contains(&"kept_analyzer".to_string()));
    assert!(reopened.sequence_state("kept_seq").unwrap().is_some());
    assert!(reopened.load_model("kept_model").unwrap().is_some());
    assert!(reopened
        .load_scoring_params("kept_params")
        .unwrap()
        .is_some());
}
