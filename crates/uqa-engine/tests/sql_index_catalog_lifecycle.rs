//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end coverage for SQL index catalog/physical-state ownership.

use std::path::Path;

use rusqlite::{params, Connection};
use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::RelationIdentity;

fn physical_relation_name(table: &str) -> String {
    RelationIdentity::from_legacy_name(table)
        .unwrap()
        .qualified_name()
}

fn ids(result: &uqa_sql::SQLResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(id)) => *id,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect()
}

fn storage_count(db: &Path, storage_table: &str, table: &str, field: &str) -> i64 {
    let table = physical_relation_name(table);
    let connection = Connection::open(db).unwrap();
    let sql = format!("SELECT COUNT(*) FROM {storage_table} WHERE table_name = ?1 AND field = ?2");
    connection
        .query_row(&sql, params![table, field], |row| row.get(0))
        .unwrap()
}

fn catalog_index_count(db: &Path, table: &str) -> i64 {
    let table = physical_relation_name(table);
    Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM _catalog_indexes WHERE table_name = ?1",
            [table],
            |row| row.get(0),
        )
        .unwrap()
}

fn persisted_fts_fields(db: &Path, table: &str) -> Vec<String> {
    let relation = RelationIdentity::from_legacy_name(table).unwrap();
    let raw: String = Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT fts_fields FROM _tables
             WHERE schema_name = ?1 AND relation_name = ?2",
            params![relation.schema, relation.name],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn assert_no_text_index(engine: &Engine, table: &str, field: &str) {
    let error = engine
        .sql(
            &format!("SELECT id FROM {table} WHERE text_match({field}, 'alpha')"),
            &[],
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("no text index"),
        "expected an unindexed-field error, got {error}"
    );
}

fn assert_shared_gin_is_live(engine: &Engine) {
    let hits = engine
        .sql(
            "SELECT id FROM docs WHERE text_match(body, 'alpha') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(ids(&hits), vec![1]);
    let stats = engine
        .sql("SELECT field, analyzer FROM fts_index_stats('docs')", &[])
        .unwrap();
    assert_eq!(stats.rows.len(), 1);
    assert_eq!(stats.rows[0]["field"], Value::Str("body".into()));
    assert_eq!(stats.rows[0]["analyzer"], Value::Str("standard_cjk".into()));
    assert_eq!(
        engine.table_field_analyzer("docs", "body").unwrap(),
        Some(("standard_cjk".into(), "both".into()))
    );
}

fn create_unnamed_index_fixture(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE items (
                id INTEGER PRIMARY KEY,
                qty INTEGER,
                body TEXT,
                embedding VECTOR(2)
            )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO items (id, qty, body, embedding) VALUES
             (1, 10, 'alpha token', ARRAY[1.0, 0.0]),
             (2, 20, 'beta token', ARRAY[0.0, 1.0]),
             (3, 10, 'gamma token', ARRAY[0.8, 0.2])",
            &[],
        )
        .unwrap();

    engine.sql("CREATE INDEX ON items (qty)", &[]).unwrap();
    engine.sql("CREATE INDEX ON items (qty)", &[]).unwrap();
    engine
        .sql("CREATE INDEX ON items USING gin (body)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE INDEX ON items USING ivf (embedding)
             WITH (lists = 1, probes = 1, train_threshold = 1)",
            &[],
        )
        .unwrap();
}

fn assert_unnamed_index_catalog(engine: &Engine) {
    let indexes = engine.list_catalog_indexes().unwrap();
    let names_and_types = indexes
        .iter()
        .map(|row| (row.name.as_str(), row.index_type.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        names_and_types,
        vec![
            ("items_body_idx", "gin"),
            ("items_embedding_idx", "ivf"),
            ("items_qty_idx", "btree"),
            ("items_qty_idx_1", "btree"),
        ]
    );
}

fn assert_unnamed_indexes_execute(engine: &Engine) {
    let scalar = engine
        .sql("SELECT id FROM items WHERE qty = 10 ORDER BY id", &[])
        .unwrap();
    assert_eq!(ids(&scalar), vec![1, 3]);
    let text = engine
        .sql(
            "SELECT id FROM items WHERE text_match(body, 'alpha') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(ids(&text), vec![1]);
    let nearest = engine
        .knn_search("items", "embedding", vec![1.0, 0.0], 1)
        .unwrap();
    assert_eq!(nearest.first().map(|hit| hit.doc_id), Some(1));
}

fn assert_unnamed_index_storage(db: &Path) {
    assert_eq!(catalog_index_count(db, "items"), 4);
    assert_eq!(storage_count(db, "_btree_indexes", "items", "qty"), 1);
    assert_eq!(storage_count(db, "_btree_index_entries", "items", "qty"), 3);
    assert!(storage_count(db, "_postings", "items", "body") > 0);
    assert_eq!(storage_count(db, "_ivf_indexes", "items", "embedding"), 1);
}

#[test]
fn unsupported_access_methods_have_no_current_or_reopen_side_effects() {
    let directory = TempDir::new().unwrap();
    let db = directory.path().join("unsupported-index.db");
    {
        let engine = Engine::open(&db).unwrap();
        engine
            .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
            .unwrap();
        engine
            .sql("INSERT INTO docs (id, body) VALUES (1, 'alpha')", &[])
            .unwrap();

        for sql in [
            "CREATE INDEX docs_body_hash ON docs USING hash (body)",
            "CREATE INDEX ON docs USING gist (body)",
        ] {
            let error = engine.sql(sql, &[]).unwrap_err();
            assert!(
                error.to_string().contains("access method")
                    && error.to_string().contains("not supported"),
                "unexpected CREATE INDEX error: {error}"
            );
        }

        assert!(engine.list_catalog_indexes().unwrap().is_empty());
        assert!(engine
            .sql("SELECT * FROM fts_index_stats('docs')", &[])
            .unwrap()
            .rows
            .is_empty());
        assert_eq!(engine.table_field_analyzer("docs", "body").unwrap(), None);
        assert_no_text_index(&engine, "docs", "body");
        assert_eq!(catalog_index_count(&db, "docs"), 0);
        assert!(persisted_fts_fields(&db, "docs").is_empty());
        assert_eq!(storage_count(&db, "_postings", "docs", "body"), 0);
    }

    let reopened = Engine::open(&db).unwrap();
    assert!(reopened.list_catalog_indexes().unwrap().is_empty());
    assert_eq!(reopened.table_field_analyzer("docs", "body").unwrap(), None);
    assert_no_text_index(&reopened, "docs", "body");
    assert_eq!(catalog_index_count(&db, "docs"), 0);
    assert!(persisted_fts_fields(&db, "docs").is_empty());
}

#[test]
fn dropping_shared_gin_cleans_physical_state_only_after_the_last_reference() {
    let directory = TempDir::new().unwrap();
    let db = directory.path().join("shared-gin.db");
    {
        let engine = Engine::open(&db).unwrap();
        engine
            .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
            .unwrap();
        engine
            .sql(
                "INSERT INTO docs (id, body) VALUES
                 (1, 'alpha token'), (2, 'beta token')",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX docs_body_gin_a ON docs USING gin (body)
                 WITH (analyzer = 'standard_cjk')",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX docs_body_gin_b ON docs USING gin (body)
                 WITH (analyzer = 'standard_cjk')",
                &[],
            )
            .unwrap();

        engine.sql("DROP INDEX docs_body_gin_a", &[]).unwrap();
        assert_eq!(
            engine
                .list_catalog_indexes()
                .unwrap()
                .into_iter()
                .map(|row| row.name)
                .collect::<Vec<_>>(),
            vec!["docs_body_gin_b"]
        );
        assert_shared_gin_is_live(&engine);
        assert!(storage_count(&db, "_postings", "docs", "body") > 0);
        assert!(storage_count(&db, "_doc_lengths", "docs", "body") > 0);
        assert!(storage_count(&db, "_field_stats", "docs", "body") > 0);
        assert_eq!(
            storage_count(&db, "_table_field_analyzers", "docs", "body"),
            1
        );
        assert_eq!(persisted_fts_fields(&db, "docs"), vec!["body"]);
    }

    {
        let engine = Engine::open(&db).unwrap();
        assert_shared_gin_is_live(&engine);
        engine.sql("DROP INDEX docs_body_gin_b", &[]).unwrap();

        assert!(engine.list_catalog_indexes().unwrap().is_empty());
        assert!(engine
            .sql("SELECT * FROM fts_index_stats('docs')", &[])
            .unwrap()
            .rows
            .is_empty());
        assert_eq!(engine.table_field_analyzer("docs", "body").unwrap(), None);
        assert_no_text_index(&engine, "docs", "body");
        for storage_table in ["_postings", "_doc_lengths", "_field_stats"] {
            assert_eq!(storage_count(&db, storage_table, "docs", "body"), 0);
        }
        assert_eq!(
            storage_count(&db, "_table_field_analyzers", "docs", "body"),
            0
        );
        assert!(persisted_fts_fields(&db, "docs").is_empty());
    }

    let reopened = Engine::open(&db).unwrap();
    assert!(reopened.list_catalog_indexes().unwrap().is_empty());
    assert!(reopened
        .sql("SELECT * FROM fts_index_stats('docs')", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(reopened.table_field_analyzer("docs", "body").unwrap(), None);
    assert_no_text_index(&reopened, "docs", "body");
}

#[test]
fn unnamed_supported_indexes_get_catalog_names_and_survive_reopen() {
    let directory = TempDir::new().unwrap();
    let db = directory.path().join("unnamed-indexes.db");
    {
        let engine = Engine::open(&db).unwrap();
        create_unnamed_index_fixture(&engine);
        assert_unnamed_index_catalog(&engine);
        assert_unnamed_indexes_execute(&engine);
        assert_unnamed_index_storage(&db);
    }

    let reopened = Engine::open(&db).unwrap();
    assert_unnamed_index_catalog(&reopened);
    assert_unnamed_indexes_execute(&reopened);
    assert_unnamed_index_storage(&db);
}
