use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use uqa_core::{Value, Vertex};
use uqa_engine::Engine;
use uqa_graph::GraphStore as _;
use uqa_ml::{DeepLayerSpec, DeepModel, GatingSpec};
use uqa_storage::document_store::Document;
use uqa_storage::{
    Catalog, CatalogFacade, ManagedConnection, PersistentStorageBackend, SQLiteStorageBackend,
};

fn persistent_engine() -> (TempDir, ManagedConnection, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(connection.clone()).unwrap());
    let backend: Arc<dyn PersistentStorageBackend> =
        Arc::new(SQLiteStorageBackend::new(connection.clone()));
    let engine = Engine::from_persistent_backends(catalog, backend).unwrap();
    (dir, connection, engine)
}

fn fail_event(connection: &ManagedConnection, table: &str, event: &str) {
    connection
        .with(|conn| {
            conn.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS injected_catalog_failure;
                 CREATE TRIGGER injected_catalog_failure
                 BEFORE {event} ON {table}
                 BEGIN SELECT RAISE(FAIL, 'injected catalog failure'); END;"
            ))?;
            Ok(())
        })
        .unwrap();
}

fn clear_failure(connection: &ManagedConnection) {
    connection
        .with(|conn| {
            conn.execute_batch("DROP TRIGGER IF EXISTS injected_catalog_failure")?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn empty_schema_and_public_are_durable_catalog_objects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schemas.db");
    {
        let engine = Engine::open(Path::new(&path)).unwrap();
        engine.sql("CREATE SCHEMA empty_app", &[]).unwrap();
        assert!(engine.has_schema("public").unwrap());
        assert!(engine.has_schema("empty_app").unwrap());
    }
    {
        let reopened = Engine::open(Path::new(&path)).unwrap();
        assert_eq!(
            reopened.list_schemas().unwrap(),
            vec!["empty_app".to_string(), "public".to_string()]
        );
        assert!(reopened.tables_in_schema("empty_app").unwrap().is_empty());
    }
}

#[test]
fn relation_names_validate_schema_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("namespace.db");
    {
        let engine = Engine::open(Path::new(&path)).unwrap();
        assert!(engine
            .sql("CREATE TABLE missing_schema.t (id INTEGER)", &[])
            .is_err());
        assert!(!engine.has_table("missing_schema.t").unwrap());

        engine.sql("CREATE SCHEMA app", &[]).unwrap();
        engine
            .sql("CREATE TABLE app.items (id INTEGER)", &[])
            .unwrap();
        assert!(engine.drop_schema("app").is_err());
        assert!(engine.drop_table("app.items").unwrap());
        assert!(engine.drop_schema("app").unwrap());
    }
    let reopened = Engine::open(Path::new(&path)).unwrap();
    assert!(!reopened.has_schema("app").unwrap());
    assert!(reopened.has_schema("public").unwrap());
}

#[test]
fn sqlite_reopen_preserves_structural_ownership_for_every_relation_kind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relation-kinds.db");
    {
        let engine = Engine::open(Path::new(&path)).unwrap();
        engine.sql("CREATE SCHEMA app", &[]).unwrap();
        engine
            .sql("CREATE TABLE app.items (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        engine.sql("INSERT INTO app.items VALUES (1)", &[]).unwrap();
        engine
            .sql("CREATE VIEW app.answer AS SELECT 42 AS value", &[])
            .unwrap();
        engine
            .sql("CREATE SEQUENCE app.item_seq START 10", &[])
            .unwrap();
        engine
            .sql(
                "CREATE SERVER app_mem FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE FOREIGN TABLE app.remote_items (id INTEGER) SERVER app_mem",
                &[],
            )
            .unwrap();
    }

    let reopened = Engine::open(Path::new(&path)).unwrap();
    assert_eq!(
        reopened.sql("SELECT id FROM app.items", &[]).unwrap().rows[0]["id"],
        Value::Int(1)
    );
    assert_eq!(
        reopened
            .sql("SELECT value FROM app.answer", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(42)
    );
    assert_eq!(
        reopened
            .sql("SELECT nextval('app.item_seq') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(10)
    );
    assert!(reopened
        .foreign_table("app.remote_items")
        .unwrap()
        .is_some());
    assert!(reopened.drop_schema("app").is_err());

    let connection = ManagedConnection::open(&path).unwrap();
    connection
        .with(|conn| {
            let rows: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _relations WHERE schema_name = 'app'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(rows, 4);
            let kinds: String = conn.query_row(
                "SELECT group_concat(kind, ',') FROM (
                     SELECT kind FROM _relations WHERE schema_name = 'app' ORDER BY kind
                 )",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(kinds, "foreign_table,sequence,table,view");
            Ok(())
        })
        .unwrap();
}

fn populate_quoted_dot_relations(engine: &Engine) {
    for statement in [
        "CREATE SCHEMA \"a.b\"",
        "CREATE SCHEMA a",
        "CREATE TABLE \"a.b\".c (id INTEGER PRIMARY KEY)",
        "CREATE TABLE a.\"b.c\" (id INTEGER PRIMARY KEY)",
        "CREATE TABLE \"a.b\" (id INTEGER PRIMARY KEY)",
        "CREATE TABLE a.b (id INTEGER PRIMARY KEY)",
        "INSERT INTO \"a.b\".c VALUES (11)",
        "INSERT INTO a.\"b.c\" VALUES (22)",
        "INSERT INTO \"a.b\" VALUES (33)",
        "INSERT INTO a.b VALUES (44)",
        "ALTER TABLE \"a.b\".c RENAME TO \"d.e\"",
        "CREATE VIEW \"a.b\".\"v.one\" AS SELECT id FROM \"a.b\".\"d.e\"",
        "CREATE SEQUENCE a.\"s.one\" START 7",
        "CREATE SERVER quoted_mem FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "CREATE FOREIGN TABLE a.\"f.one\" (id INTEGER) SERVER quoted_mem",
    ] {
        engine
            .sql(statement, &[])
            .unwrap_or_else(|error| panic!("quoted-dot setup failed for `{statement}`: {error}"));
    }
    assert!(engine.sql("CREATE SEQUENCE \"a.b\".\"d.e\"", &[]).is_err());
}

fn assert_quoted_dot_relation_values(engine: &Engine) {
    for (relation, expected) in [
        ("\"a.b\".\"d.e\"", 11),
        ("a.\"b.c\"", 22),
        ("\"a.b\"", 33),
        ("a.b", 44),
    ] {
        let result = engine
            .sql(&format!("SELECT id FROM {relation}"), &[])
            .unwrap();
        assert_eq!(result.rows[0]["id"], Value::Int(expected));
    }
    assert_eq!(
        engine
            .sql("SELECT id FROM \"a.b\".\"v.one\"", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(11)
    );
    assert_eq!(
        engine
            .sql("SELECT nextval('a.\"s.one\"') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(7)
    );
    assert!(engine.foreign_table("a.\"f.one\"").unwrap().is_some());
}

fn assert_structural_table_identities(path: &Path) {
    let connection = ManagedConnection::open(path).unwrap();
    let identities = connection
        .with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT schema_name, relation_name FROM _relations WHERE kind = 'table' \
                 ORDER BY schema_name, relation_name",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .unwrap();
    assert_eq!(
        identities,
        vec![
            ("a".to_string(), "b".to_string()),
            ("a".to_string(), "b.c".to_string()),
            ("a.b".to_string(), "d.e".to_string()),
            ("public".to_string(), "a.b".to_string()),
        ]
    );
}

#[test]
fn quoted_dot_relations_are_distinct_through_rename_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("quoted-dot-relations.db");
    populate_quoted_dot_relations(&Engine::open(Path::new(&path)).unwrap());
    assert_quoted_dot_relation_values(&Engine::open(Path::new(&path)).unwrap());
    assert_structural_table_identities(&path);
}

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

#[test]
fn drop_table_is_atomic_across_schema_and_owned_data() {
    let (dir, connection, engine) = persistent_engine();
    engine
        .sql(
            "CREATE TABLE kept_table (id INTEGER PRIMARY KEY, body TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO kept_table VALUES (1, 'still here')", &[])
        .unwrap();

    fail_event(&connection, "_documents", "DELETE");
    assert!(engine.drop_table("kept_table").is_err());
    assert!(engine.has_table("kept_table").unwrap());
    assert_eq!(
        engine
            .get_document("kept_table", 1)
            .unwrap()
            .unwrap()
            .get("body"),
        Some(&uqa_core::Value::Str("still here".into()))
    );
    clear_failure(&connection);
    drop(engine);
    drop(connection);

    let reopened = Engine::open(&dir.path().join("catalog.db")).unwrap();
    assert!(reopened.has_table("kept_table").unwrap());
    assert_eq!(
        reopened
            .get_document("kept_table", 1)
            .unwrap()
            .unwrap()
            .get("body"),
        Some(&uqa_core::Value::Str("still here".into()))
    );
}

#[test]
fn unqualified_analyzer_assignment_uses_the_resolved_schema_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("analyzer-schema.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine.sql("CREATE SCHEMA app", &[]).unwrap();
        engine.set_search_path(vec!["app".to_string()]);
        engine
            .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
            .unwrap();
        engine
            .sql("CREATE INDEX docs_body_idx ON docs USING gin (body)", &[])
            .unwrap();
        engine
            .register_named_analyzer("strict", r#"{"tokenizer":"keyword"}"#)
            .unwrap();
        engine
            .set_table_field_analyzer("docs", "body", "strict", "both")
            .unwrap();
        assert_eq!(
            engine.table_field_analyzer("app.docs", "body").unwrap(),
            Some(("strict".to_string(), "both".to_string()))
        );
    }

    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        reopened.table_field_analyzer("app.docs", "body").unwrap(),
        Some(("strict".to_string(), "both".to_string()))
    );
}

#[test]
fn graph_mutation_and_drop_roll_back_on_catalog_failure() {
    let (_dir, connection, engine) = persistent_engine();
    engine.create_graph("g").unwrap();

    fail_event(&connection, "_named_graphs", "INSERT");
    assert!(engine
        .add_graph_vertex(Vertex::new(1, "Person"), "g")
        .is_err());
    assert_eq!(
        engine
            .graph_with("g", |graph| {
                graph.vertex_ids_in_graph("g").unwrap().len()
            })
            .unwrap()
            .unwrap(),
        0
    );
    clear_failure(&connection);

    fail_event(&connection, "_named_graphs", "DELETE");
    assert!(engine.drop_graph("g").is_err());
    assert!(engine.has_graph("g").unwrap());
}

#[test]
fn graph_edges_cannot_publish_dangling_endpoints() {
    let engine = Engine::new();
    engine.create_graph("g").unwrap();
    engine
        .add_graph_vertex(Vertex::new(1, "Person"), "g")
        .unwrap();
    let dangling = uqa_core::Edge::new(9, 1, 2, "KNOWS");
    assert!(engine.add_graph_edge(dangling, "g").is_err());
    assert_eq!(
        engine
            .graph_with("g", |graph| graph.edges_in_graph("g").unwrap().len())
            .unwrap(),
        Some(0)
    );
}

#[test]
fn analyze_does_not_publish_stats_when_catalog_write_fails() {
    let (_dir, connection, engine) = persistent_engine();
    engine.sql("CREATE TABLE stats_t (x INTEGER)", &[]).unwrap();
    engine.sql("INSERT INTO stats_t VALUES (1)", &[]).unwrap();
    engine.run_analyze(Some("stats_t")).unwrap();
    let before = engine.column_stats("stats_t").unwrap();

    fail_event(&connection, "_column_stats", "DELETE");
    assert!(engine.run_analyze(Some("stats_t")).is_err());
    let after = engine.column_stats("stats_t").unwrap();
    assert_eq!(after["x"].row_count, before["x"].row_count);
    assert_eq!(after["x"].distinct_count, before["x"].distinct_count);
}

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
