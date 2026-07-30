//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine coverage for the SQLite-backed `KeyValue` storage path.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value as JSONValue;
use tempfile::tempdir;
use uqa_core::{Edge, Value, Vertex};
use uqa_engine::{Engine, ScoringMode};
use uqa_graph::GraphStore as _;
use uqa_storage::{CatalogFacade, ManagedConnection, PersistentStorageBackend};
use uqa_storage_sqlite::SQLiteKeyValueStorage;

#[derive(Deserialize)]
struct Fixture {
    #[allow(dead_code)]
    version: u32,
    schema_sql: Vec<String>,
    data_sql: Vec<String>,
    cases: Vec<GoldenCase>,
}

#[derive(Deserialize)]
struct GoldenCase {
    name: String,
    sql: String,
    expected: Vec<JSONValue>,
}

fn open_key_value_engine(path: &std::path::Path) -> Engine {
    open_key_value_storage_and_engine(path).1
}

fn open_key_value_storage_and_engine(path: &std::path::Path) -> (SQLiteKeyValueStorage, Engine) {
    let storage = SQLiteKeyValueStorage::open(path).expect("open SQLite KeyValue storage");
    let catalog: Arc<dyn CatalogFacade> = Arc::new(storage.catalog());
    let backend: Arc<dyn PersistentStorageBackend> = Arc::new(storage.backend());
    let engine = Engine::from_persistent_backends(catalog, backend).expect("open KeyValue engine");
    (storage, engine)
}

fn fail_nth_key_value_insert(connection: &ManagedConnection, nth: usize) {
    assert!(
        nth > 0,
        "fault injection requires a positive operation index"
    );
    connection
        .with(|sqlite| {
            sqlite.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS injected_key_value_insert_failure;
                 CREATE TABLE IF NOT EXISTS _key_value_fault_counter (
                     remaining INTEGER NOT NULL
                 );
                 DELETE FROM _key_value_fault_counter;
                 INSERT INTO _key_value_fault_counter (remaining) VALUES ({nth});
                 CREATE TRIGGER injected_key_value_insert_failure
                 BEFORE INSERT ON _key_value
                 BEGIN
                     UPDATE _key_value_fault_counter
                     SET remaining = remaining - 1;
                     SELECT CASE
                         WHEN (SELECT remaining FROM _key_value_fault_counter) = 0
                         THEN RAISE(FAIL, 'injected KeyValue insert failure')
                     END;
                 END;"
            ))?;
            Ok(())
        })
        .expect("install KeyValue fault trigger");
}

fn clear_key_value_insert_failure(connection: &ManagedConnection) {
    connection
        .with(|sqlite| {
            sqlite.execute_batch(
                "DROP TRIGGER IF EXISTS injected_key_value_insert_failure;
                 DROP TABLE IF EXISTS _key_value_fault_counter;",
            )?;
            Ok(())
        })
        .expect("clear KeyValue fault trigger");
}

fn fixture_path() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests")
        .join("parity")
        .join("sql_golden_fixture.json")
}

fn load_fixture() -> Fixture {
    let bytes = std::fs::read(fixture_path()).expect("fixture present");
    serde_json::from_slice(&bytes).expect("fixture parses")
}

fn json_to_value(v: &JSONValue) -> Value {
    match v {
        JSONValue::Null | JSONValue::Object(_) => Value::Null,
        JSONValue::Bool(b) => Value::Bool(*b),
        JSONValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        JSONValue::String(s) => Value::Str(s.clone()),
        JSONValue::Array(items) => Value::List(items.iter().map(json_to_value).collect()),
    }
}

#[test]
fn engine_runs_text_and_vector_workloads_on_sqlite_key_value_storage() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-engine.sqlite3");

    {
        let engine = open_key_value_engine(&path);
        engine
            .sql(
                "CREATE TABLE articles (
                    id INTEGER PRIMARY KEY,
                    title TEXT,
                    embedding VECTOR(2)
                )",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX articles_title_gin ON articles USING gin (title)",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO articles (id, title, embedding) VALUES
                 (1, 'rust search engine', ARRAY[1.0, 0.0]),
                 (2, 'sqlite key value', ARRAY[0.0, 1.0])",
                &[],
            )
            .unwrap();

        let hits = engine
            .search("articles", "title", "rust", &ScoringMode::default(), 10)
            .unwrap();
        assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
        let vector_hits = engine
            .knn_search("articles", "embedding", vec![1.0, 0.0], 1)
            .unwrap();
        assert_eq!(vector_hits.first().map(|hit| hit.doc_id), Some(1));
    }

    let reopened = open_key_value_engine(&path);
    let got = reopened.get_document("articles", 2).unwrap().unwrap();
    assert_eq!(
        got.get("title"),
        Some(&uqa_core::Value::Str("sqlite key value".into()))
    );
    let hits = reopened
        .search("articles", "title", "sqlite", &ScoringMode::default(), 10)
        .unwrap();
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(2));
    let vector_hits = reopened
        .knn_search("articles", "embedding", vec![0.0, 1.0], 1)
        .unwrap();
    assert_eq!(vector_hits.first().map(|hit| hit.doc_id), Some(2));
}

#[test]
fn update_to_fts_column_refreshes_sqlite_key_value_postings() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-update.sqlite3");

    {
        let engine = open_key_value_engine(&path);
        engine
            .sql(
                "CREATE TABLE messages (
                    id INTEGER PRIMARY KEY,
                    public_id TEXT UNIQUE,
                    content TEXT
                )",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX messages_content_gin ON messages USING gin (content)",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO messages (id, public_id, content) VALUES
                 (1, 'm-1', 'old alpha'),
                 (2, 'm-2', 'old beta')",
                &[],
            )
            .unwrap();

        let updated = engine
            .sql(
                "UPDATE messages SET content = 'new gamma' WHERE public_id = 'm-2'",
                &[],
            )
            .unwrap();
        assert_eq!(updated.affected_rows, 1);

        let old_hits = engine
            .sql(
                "SELECT id FROM messages WHERE text_match(content, 'beta') ORDER BY id",
                &[],
            )
            .unwrap();
        assert!(old_hits.rows.is_empty());
        let new_hits = engine
            .sql(
                "SELECT public_id FROM messages WHERE text_match(content, 'gamma')",
                &[],
            )
            .unwrap();
        assert_eq!(new_hits.rows.len(), 1);
        assert_eq!(
            new_hits.rows[0].get("public_id"),
            Some(&Value::Str("m-2".into()))
        );
    }

    let reopened = open_key_value_engine(&path);
    let old_hits = reopened
        .sql(
            "SELECT id FROM messages WHERE text_match(content, 'beta') ORDER BY id",
            &[],
        )
        .unwrap();
    assert!(old_hits.rows.is_empty());
    let new_hits = reopened
        .sql(
            "SELECT public_id FROM messages WHERE text_match(content, 'gamma')",
            &[],
        )
        .unwrap();
    assert_eq!(new_hits.rows.len(), 1);
    assert_eq!(
        new_hits.rows[0].get("public_id"),
        Some(&Value::Str("m-2".into()))
    );
}

#[test]
fn update_to_fts_column_refreshes_all_non_unique_matches() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-update-many.sqlite3");
    let engine = open_key_value_engine(&path);
    engine
        .sql(
            "CREATE TABLE messages (
                id INTEGER PRIMARY KEY,
                room TEXT,
                content TEXT
            )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX messages_content_gin ON messages USING gin (content)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO messages (id, room, content) VALUES
             (1, 'chat', 'old alpha'),
             (2, 'chat', 'old beta'),
             (3, 'note', 'old beta')",
            &[],
        )
        .unwrap();

    let updated = engine
        .sql(
            "UPDATE messages SET content = 'fresh gamma' WHERE room = 'chat'",
            &[],
        )
        .unwrap();
    assert_eq!(updated.affected_rows, 2);

    let old_chat_hits = engine
        .sql(
            "SELECT id FROM messages
             WHERE text_match(content, 'old') AND room = 'chat'
             ORDER BY id",
            &[],
        )
        .unwrap();
    assert!(old_chat_hits.rows.is_empty());
    let new_hits = engine
        .sql(
            "SELECT id FROM messages WHERE text_match(content, 'gamma') ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = new_hits
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(id)) => *id,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn sql_golden_fixture_passes_on_sqlite_key_value_storage() {
    let fixture = load_fixture();
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-golden.sqlite3");
    let engine = open_key_value_engine(&path);

    for stmt in &fixture.schema_sql {
        engine.sql(stmt, &[]).expect("schema sql");
    }
    for stmt in &fixture.data_sql {
        engine.sql(stmt, &[]).expect("data sql");
    }
    for case in &fixture.cases {
        let result = engine
            .sql(&case.sql, &[])
            .unwrap_or_else(|err| panic!("[{}] sql error: {err}", case.name));
        assert_eq!(
            result.rows.len(),
            case.expected.len(),
            "[{}] row count differs",
            case.name
        );
        for (i, (got, expected)) in result.rows.iter().zip(case.expected.iter()).enumerate() {
            let expected = expected.as_object().unwrap_or_else(|| {
                panic!(
                    "[{}] expected row {i} is not an object: {expected}",
                    case.name
                )
            });
            for (column, expected_json) in expected {
                let expected = json_to_value(expected_json);
                let actual = got.get(column).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expected,
                    "[{} idx {i}] column {column:?}: got {:?}, expected {:?}",
                    case.name, actual, expected
                );
            }
        }
    }
}

#[test]
fn empty_schema_survives_key_value_engine_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-schemas.sqlite3");
    {
        let engine = open_key_value_engine(&path);
        engine.sql("CREATE SCHEMA empty_app", &[]).unwrap();
        assert!(engine.has_schema("public").unwrap());
        assert!(engine.has_schema("empty_app").unwrap());
    }
    let reopened = open_key_value_engine(&path);
    assert_eq!(
        reopened.list_schemas().unwrap(),
        vec!["empty_app".to_string(), "public".to_string()]
    );
}

#[test]
fn key_value_reopen_preserves_every_schema_owned_relation_kind() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-relation-kinds.sqlite3");
    {
        let engine = open_key_value_engine(&path);
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

    let reopened = open_key_value_engine(&path);
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
}

#[test]
fn key_value_reopen_keeps_quoted_dot_relation_identities_isolated() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-quoted-dot-relations.sqlite3");
    {
        let engine = open_key_value_engine(&path);
        engine.sql("CREATE SCHEMA \"a.b\"", &[]).unwrap();
        engine.sql("CREATE SCHEMA a", &[]).unwrap();
        engine
            .sql("CREATE TABLE \"a.b\".c (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        engine
            .sql("CREATE TABLE a.\"b.c\" (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        engine.sql("INSERT INTO \"a.b\".c VALUES (1)", &[]).unwrap();
        engine.sql("INSERT INTO a.\"b.c\" VALUES (2)", &[]).unwrap();
        engine
            .sql("ALTER TABLE \"a.b\".c RENAME TO \"d.e\"", &[])
            .unwrap();
        engine
            .sql(
                "CREATE VIEW \"a.b\".\"v.one\" AS SELECT id FROM \"a.b\".\"d.e\"",
                &[],
            )
            .unwrap();
        engine
            .sql("CREATE SEQUENCE a.\"s.one\" START 5", &[])
            .unwrap();
    }

    let reopened = open_key_value_engine(&path);
    assert_eq!(
        reopened
            .sql("SELECT id FROM \"a.b\".\"d.e\"", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
    assert_eq!(
        reopened.sql("SELECT id FROM a.\"b.c\"", &[]).unwrap().rows[0]["id"],
        Value::Int(2)
    );
    assert_eq!(
        reopened
            .sql("SELECT id FROM \"a.b\".\"v.one\"", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
    assert_eq!(
        reopened
            .sql("SELECT nextval('a.\"s.one\"') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(5)
    );
}

#[test]
fn failed_key_value_catalog_batch_does_not_leave_an_orphan_relation() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-relation-atomicity.sqlite3");
    let (storage, engine) = open_key_value_storage_and_engine(&path);
    let connection = storage.store().connection();

    // Sequence creation first claims the shared relation name and then writes
    // the sequence payload. Failing the second write must roll both back.
    fail_nth_key_value_insert(&connection, 2);
    let error = engine
        .sql("CREATE SEQUENCE rolled_back_relation START 7", &[])
        .expect_err("the injected second batch write must abort sequence creation");
    assert!(error
        .to_string()
        .contains("injected KeyValue insert failure"));
    assert!(engine
        .sequence_state("rolled_back_relation")
        .unwrap()
        .is_none());
    clear_key_value_insert_failure(&connection);

    // Reusing the same name as another relation kind proves that the first
    // relation-claim write did not survive the failed batch.
    engine
        .sql(
            "CREATE TABLE rolled_back_relation (id INTEGER PRIMARY KEY)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO rolled_back_relation VALUES (1)", &[])
        .unwrap();

    drop(engine);
    drop(connection);
    drop(storage);
    let reopened = open_key_value_engine(&path);
    assert!(reopened
        .sequence_state("rolled_back_relation")
        .unwrap()
        .is_none());
    assert_eq!(
        reopened
            .sql("SELECT id FROM rolled_back_relation", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
}

#[test]
fn failed_key_value_graph_replacement_preserves_snapshot_and_path_index() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-graph-atomicity.sqlite3");
    let (storage, engine) = open_key_value_storage_and_engine(&path);
    let connection = storage.store().connection();

    engine.create_graph("g").unwrap();
    engine
        .add_graph_vertex(Vertex::new(1, "Person"), "g")
        .unwrap();
    engine
        .add_graph_vertex(Vertex::new(2, "Person"), "g")
        .unwrap();
    engine
        .add_graph_edge(Edge::new(10, 1, 2, "knows"), "g")
        .unwrap();
    engine
        .build_path_index("knows_idx", "g", &[vec!["knows".to_string()]])
        .unwrap();

    // Graph replacement writes the graph marker, removes old memberships and
    // dependent path indexes, then writes the replacement snapshot. Failing
    // its second INSERT proves that the preceding put and deletes are covered
    // by the same SQLite KeyValue savepoint.
    fail_nth_key_value_insert(&connection, 2);
    let error = engine
        .add_graph_vertex(Vertex::new(3, "Person"), "g")
        .expect_err("the injected graph snapshot write must abort replacement");
    assert!(error
        .to_string()
        .contains("injected KeyValue insert failure"));

    let live_vertices = engine
        .graph_with("g", |store| store.vertex_ids_in_graph("g").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(live_vertices.into_iter().collect::<Vec<_>>(), vec![1, 2]);
    assert!(engine.get_path_index("knows_idx", "g").unwrap().is_some());
    clear_key_value_insert_failure(&connection);

    drop(engine);
    drop(connection);
    drop(storage);
    let reopened = open_key_value_engine(&path);
    let reopened_vertices = reopened
        .graph_with("g", |store| store.vertex_ids_in_graph("g").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        reopened_vertices.into_iter().collect::<Vec<_>>(),
        vec![1, 2]
    );
    let index = reopened
        .get_path_index("knows_idx", "g")
        .unwrap()
        .expect("rolled-back path index must survive reopen");
    assert_eq!(
        index
            .lookup(&["knows".to_string()])
            .unwrap()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![(1, 2)]
    );
}
