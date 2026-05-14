//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use rusqlite::{params, Connection};
use serde_json::json;
use tempfile::tempdir;
use uqa_core::{TemporalValue, Value};
use uqa_engine::migration::migrate_python_database;
use uqa_engine::{Engine, ScoringMode};
use uqa_graph::GraphStore;

#[test]
fn migrates_python_uqa_catalog_from_directory() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("python_repo");
    let nested = source_dir.join("db");
    std::fs::create_dir_all(&nested).unwrap();

    let unrelated = source_dir.join("taxi.db");
    Connection::open(&unrelated)
        .unwrap()
        .execute_batch("CREATE TABLE trips (id INTEGER PRIMARY KEY)")
        .unwrap();

    let source = nested.join("uqa.sqlite");
    create_python_catalog(&source);
    let destination = tmp.path().join("uqa-rs.sqlite");

    let report = migrate_python_database(&source_dir, &destination).unwrap();
    assert_eq!(report.tables, 1);
    assert_eq!(report.documents, 2);
    assert_eq!(report.fts_fields, 2);
    assert_eq!(report.vector_fields, 1);
    assert_eq!(report.indexes, 2);
    assert_eq!(report.graphs, 1);
    assert_eq!(report.graph_vertices, 2);
    assert_eq!(report.graph_edges, 1);
    assert_eq!(report.path_indexes, 1);
    assert_eq!(report.scoring_params, 1);
    assert_eq!(report.models, 1);
    assert_eq!(report.foreign_servers, 1);
    assert_eq!(report.foreign_tables, 1);
    assert_eq!(report.column_stats, 1);

    let engine = Engine::open(&destination).unwrap();
    assert_eq!(engine.table_names(), vec!["docs".to_string()]);
    assert_eq!(
        engine.list_foreign_servers(),
        vec!["memory_srv".to_string()]
    );
    assert_eq!(
        engine.list_foreign_tables(),
        vec!["remote_docs".to_string()]
    );
    assert_eq!(engine.list_path_indexes(), vec!["g::default".to_string()]);

    let doc = engine.get_document("docs", 1).unwrap();
    assert_eq!(doc.get("title"), Some(&Value::Str("Rust migration".into())));
    assert!(matches!(doc.get("payload"), Some(Value::Map(_))));
    assert!(matches!(
        doc.get("published_at"),
        Some(Value::Temporal(TemporalValue::Timestamp { .. }))
    ));
    assert!(matches!(
        doc.get("event_date"),
        Some(Value::Temporal(TemporalValue::Date { .. }))
    ));
    assert!(matches!(
        doc.get("wake_time"),
        Some(Value::Temporal(TemporalValue::TimeTz { .. }))
    ));

    let text_hits = engine.search("docs", "body", "database", &ScoringMode::default(), 10);
    assert_eq!(text_hits.first().map(|hit| hit.doc_id), Some(1));

    let vector_hits = engine.knn_search("docs", "embedding", vec![1.0, 0.0, 0.0], 1);
    assert_eq!(vector_hits.first().map(|hit| hit.doc_id), Some(1));

    assert_eq!(
        engine.load_scoring_params("docs.body").as_deref(),
        Some("{\"alpha\":1.5}")
    );

    let stats = engine.column_stats("docs");
    assert_eq!(stats.get("id").map(|s| s.row_count), Some(2));

    let graph_counts = engine
        .graph_with("g", |store| {
            (
                store.vertices_in_graph("g").len(),
                store.edges_in_graph("g").len(),
            )
        })
        .unwrap();
    assert_eq!(graph_counts, (2, 1));
}

fn create_python_catalog(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    create_python_schema(&conn);
    insert_python_table_rows(&conn);
    insert_python_index_rows(&conn);
    insert_python_metadata_rows(&conn);
    insert_python_graph_rows(&conn);
    insert_python_foreign_rows(&conn);
}

fn create_python_schema(conn: &Connection) {
    conn.execute_batch(
        r"
        CREATE TABLE _catalog_tables (
            name TEXT PRIMARY KEY,
            columns_json TEXT NOT NULL
        );
        CREATE TABLE _data_docs (
            _rowid INTEGER PRIMARY KEY,
            id INTEGER,
            title TEXT,
            body TEXT,
            embedding TEXT,
            published_at TEXT,
            event_date TEXT,
            wake_time TEXT,
            payload TEXT
        );
        CREATE TABLE _catalog_indexes (
            name TEXT PRIMARY KEY,
            index_type TEXT NOT NULL,
            table_name TEXT NOT NULL,
            columns TEXT NOT NULL,
            parameters TEXT NOT NULL
        );
        CREATE TABLE _scoring_params (
            name TEXT PRIMARY KEY,
            params_json TEXT NOT NULL
        );
        CREATE TABLE _models (
            model_name TEXT PRIMARY KEY,
            config_json TEXT NOT NULL
        );
        CREATE TABLE _column_stats (
            table_name TEXT NOT NULL,
            column_name TEXT NOT NULL,
            distinct_count INTEGER NOT NULL DEFAULT 0,
            null_count INTEGER NOT NULL DEFAULT 0,
            min_value TEXT,
            max_value TEXT,
            row_count INTEGER NOT NULL DEFAULT 0,
            histogram TEXT NOT NULL DEFAULT '[]',
            mcv_values TEXT NOT NULL DEFAULT '[]',
            mcv_frequencies TEXT NOT NULL DEFAULT '[]',
            PRIMARY KEY (table_name, column_name)
        );
        CREATE TABLE _graph_catalog (graph_name TEXT PRIMARY KEY);
        CREATE TABLE _graph_vertices (
            vertex_id INTEGER PRIMARY KEY,
            label TEXT NOT NULL DEFAULT '',
            properties_json TEXT NOT NULL
        );
        CREATE TABLE _graph_edges (
            edge_id INTEGER PRIMARY KEY,
            source_id INTEGER NOT NULL,
            target_id INTEGER NOT NULL,
            label TEXT NOT NULL,
            properties_json TEXT NOT NULL
        );
        CREATE TABLE _graph_membership (
            entity_type TEXT NOT NULL,
            entity_id INTEGER NOT NULL,
            graph_name TEXT NOT NULL,
            PRIMARY KEY (entity_type, entity_id, graph_name)
        );
        CREATE TABLE _path_indexes (
            graph_name TEXT PRIMARY KEY,
            label_sequences TEXT NOT NULL
        );
        CREATE TABLE _foreign_servers (
            name TEXT PRIMARY KEY,
            fdw_type TEXT NOT NULL,
            options TEXT NOT NULL
        );
        CREATE TABLE _foreign_tables (
            name TEXT PRIMARY KEY,
            server_name TEXT NOT NULL,
            columns_json TEXT NOT NULL,
            options TEXT NOT NULL
        );
        ",
    )
    .unwrap();
}

fn insert_python_table_rows(conn: &Connection) {
    let columns = python_doc_columns();
    conn.execute(
        "INSERT INTO _catalog_tables (name, columns_json) VALUES (?1, ?2)",
        params!["docs", columns.to_string()],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO _data_docs
            (_rowid, id, title, body, embedding, published_at, event_date, wake_time, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            1_i64,
            1_i64,
            "Rust migration",
            "Rust database migration",
            "[1.0, 0.0, 0.0]",
            "2026-05-14 09:30:00",
            "2026-05-14",
            "09:30:00+09:00",
            "{\"kind\":\"guide\"}"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO _data_docs
            (_rowid, id, title, body, embedding, published_at, event_date, wake_time, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            2_i64,
            2_i64,
            "Python reference",
            "Python catalog reference",
            "[0.0, 1.0, 0.0]",
            "2026-05-13 10:00:00",
            "2026-05-13",
            "10:00:00+09:00",
            "{\"kind\":\"source\"}"
        ],
    )
    .unwrap();
}

fn python_doc_columns() -> serde_json::Value {
    json!([
        {
            "name": "id",
            "type_name": "integer",
            "primary_key": true,
            "not_null": true,
            "auto_increment": false,
            "default": null,
            "unique": false
        },
        {
            "name": "title",
            "type_name": "text",
            "primary_key": false,
            "not_null": false,
            "auto_increment": false,
            "default": null,
            "unique": false
        },
        {
            "name": "body",
            "type_name": "text",
            "primary_key": false,
            "not_null": false,
            "auto_increment": false,
            "default": null,
            "unique": false
        },
        {
            "name": "embedding",
            "type_name": "vector",
            "primary_key": false,
            "not_null": false,
            "auto_increment": false,
            "default": null,
            "vector_dimensions": 3,
            "unique": false
        },
        {
            "name": "payload",
            "type_name": "jsonb",
            "primary_key": false,
            "not_null": false,
            "auto_increment": false,
            "default": null,
            "unique": false
        },
        {
            "name": "published_at",
            "type_name": "timestamp without time zone",
            "primary_key": false,
            "not_null": false,
            "auto_increment": false,
            "default": null,
            "unique": false
        },
        {
            "name": "event_date",
            "type_name": "date",
            "primary_key": false,
            "not_null": false,
            "auto_increment": false,
            "default": null,
            "unique": false
        },
        {
            "name": "wake_time",
            "type_name": "timetz",
            "primary_key": false,
            "not_null": false,
            "auto_increment": false,
            "default": null,
            "unique": false
        }
    ])
}

fn insert_python_index_rows(conn: &Connection) {
    conn.execute(
        "INSERT INTO _catalog_indexes (name, index_type, table_name, columns, parameters)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params!["idx_docs_gin", "gin", "docs", "[\"title\",\"body\"]", "{}"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO _catalog_indexes (name, index_type, table_name, columns, parameters)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            "idx_docs_embedding",
            "ivf",
            "docs",
            "[\"embedding\"]",
            "{\"nlist\":2,\"nprobe\":1}"
        ],
    )
    .unwrap();
}

fn insert_python_metadata_rows(conn: &Connection) {
    conn.execute(
        "INSERT INTO _scoring_params (name, params_json) VALUES (?1, ?2)",
        params!["docs.body", "{\"alpha\":1.5}"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO _models (model_name, config_json) VALUES (?1, ?2)",
        params!["toy", "{\"layers\":[]}"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO _column_stats
            (table_name, column_name, distinct_count, null_count, min_value, max_value,
             row_count, histogram, mcv_values, mcv_frequencies)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params!["docs", "id", 2_i64, 0_i64, "1", "2", 2_i64, "[]", "[]", "[]"],
    )
    .unwrap();
}

fn insert_python_graph_rows(conn: &Connection) {
    conn.execute(
        "INSERT INTO _graph_catalog (graph_name) VALUES (?1)",
        params!["g"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO _graph_vertices (vertex_id, label, properties_json)
         VALUES (?1, ?2, ?3)",
        params![1_i64, "Doc", "{\"title\":\"Rust migration\"}"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO _graph_vertices (vertex_id, label, properties_json)
         VALUES (?1, ?2, ?3)",
        params![2_i64, "Doc", "{\"title\":\"Python reference\"}"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO _graph_edges (edge_id, source_id, target_id, label, properties_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![10_i64, 1_i64, 2_i64, "links", "{}"],
    )
    .unwrap();
    for (entity_type, entity_id) in [("vertex", 1_i64), ("vertex", 2_i64), ("edge", 10_i64)] {
        conn.execute(
            "INSERT INTO _graph_membership (entity_type, entity_id, graph_name)
             VALUES (?1, ?2, ?3)",
            params![entity_type, entity_id, "g"],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO _path_indexes (graph_name, label_sequences) VALUES (?1, ?2)",
        params!["g", "[[\"links\"]]"],
    )
    .unwrap();
}

fn insert_python_foreign_rows(conn: &Connection) {
    conn.execute(
        "INSERT INTO _foreign_servers (name, fdw_type, options) VALUES (?1, ?2, ?3)",
        params!["memory_srv", "memory_fdw", "{}"],
    )
    .unwrap();
    let foreign_columns = json!([
        {
            "name": "id",
            "type_name": "integer",
            "primary_key": false,
            "not_null": false,
            "auto_increment": false
        }
    ]);
    conn.execute(
        "INSERT INTO _foreign_tables (name, server_name, columns_json, options)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            "remote_docs",
            "memory_srv",
            foreign_columns.to_string(),
            "{}"
        ],
    )
    .unwrap();
}
