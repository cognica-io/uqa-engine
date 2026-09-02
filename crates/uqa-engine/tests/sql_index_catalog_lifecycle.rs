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
    let table = RelationIdentity::from_legacy_name(table).unwrap();
    Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM _catalog_indexes
              WHERE table_schema_name = ?1 AND table_relation_name = ?2",
            params![table.schema, table.name],
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

#[test]
fn fts_index_stats_rejects_unknown_table_filter() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);
             CREATE INDEX docs_body_gin ON docs USING gin (body)",
            &[],
        )
        .unwrap();

    let error = engine
        .sql("SELECT * FROM fts_index_stats('missing')", &[])
        .unwrap_err();
    assert!(
        matches!(error, uqa_sql::SQLError::UnknownTable(ref name) if name == "missing"),
        "expected unknown table, got {error}"
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
        .map(|row| (row.relation.name.as_str(), row.index_type.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        names_and_types,
        vec![
            ("items_body_idx", "gin"),
            ("items_embedding_idx", "ivf"),
            ("items_qty_idx", "btree"),
            ("items_qty_idx1", "btree"),
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

fn index_identities(engine: &Engine) -> Vec<String> {
    engine
        .list_catalog_indexes()
        .unwrap()
        .into_iter()
        .map(|row| row.relation.qualified_name())
        .collect()
}

#[test]
fn schema_scoped_index_identities_keep_same_local_names_and_regclass_oids() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("schema-index-identities.db");
    let first_oid;
    let second_oid;
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE SCHEMA index_alpha;
                 CREATE SCHEMA index_beta;
                 CREATE TABLE index_alpha.items(id integer);
                 CREATE TABLE index_beta.items(id integer);
                 CREATE INDEX shared_idx ON index_alpha.items(id);
                 CREATE INDEX shared_idx ON index_beta.items(id)",
                &[],
            )
            .unwrap();
        assert_eq!(
            index_identities(&engine),
            ["index_alpha.shared_idx", "index_beta.shared_idx"]
        );
        let oids = engine
            .sql(
                "SELECT 'index_alpha.shared_idx'::regclass::oid AS alpha_oid,
                        'index_beta.shared_idx'::regclass::oid AS beta_oid",
                &[],
            )
            .unwrap();
        let Value::Int(alpha_oid) = oids.rows[0]["alpha_oid"] else {
            panic!("alpha index regclass must use an integer OID")
        };
        let Value::Int(beta_oid) = oids.rows[0]["beta_oid"] else {
            panic!("beta index regclass must use an integer OID")
        };
        assert_ne!(alpha_oid, beta_oid);
        first_oid = alpha_oid;
        second_oid = beta_oid;
    }

    {
        let engine = Engine::open(&database).unwrap();
        assert_eq!(
            index_identities(&engine),
            ["index_alpha.shared_idx", "index_beta.shared_idx"]
        );
        let oids = engine
            .sql(
                "SELECT 'index_alpha.shared_idx'::regclass::oid AS alpha_oid,
                        'index_beta.shared_idx'::regclass::oid AS beta_oid",
                &[],
            )
            .unwrap();
        assert_eq!(oids.rows[0]["alpha_oid"], Value::Int(first_oid));
        assert_eq!(oids.rows[0]["beta_oid"], Value::Int(second_oid));
        engine
            .sql(
                "SET search_path TO index_beta, index_alpha; DROP INDEX shared_idx",
                &[],
            )
            .unwrap();
        assert_eq!(index_identities(&engine), ["index_alpha.shared_idx"]);
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(index_identities(&reopened), ["index_alpha.shared_idx"]);
    assert_eq!(
        reopened
            .sql(
                "SELECT to_regclass('index_beta.shared_idx') IS NULL AS missing",
                &[]
            )
            .unwrap()
            .rows[0]["missing"],
        Value::Bool(true)
    );
}

#[test]
fn temporary_indexes_never_enter_the_durable_catalog() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("temporary-index.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE TEMP TABLE temp_items(id integer);
                 CREATE INDEX temp_items_idx ON temp_items(id)",
                &[],
            )
            .unwrap();
        assert_eq!(
            index_identities(&engine)
                .into_iter()
                .map(|name| name.rsplit_once('.').unwrap().1.to_string())
                .collect::<Vec<_>>(),
            ["temp_items_idx"]
        );
        let count: i64 = Connection::open(&database)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM _catalog_indexes", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    let reopened = Engine::open(&database).unwrap();
    assert!(index_identities(&reopened).is_empty());
    assert_eq!(
        reopened
            .sql(
                "SELECT to_regclass('temp_items_idx') IS NULL AS missing",
                &[]
            )
            .unwrap()
            .rows[0]["missing"],
        Value::Bool(true)
    );
}

#[test]
fn temporary_indexes_survive_persistent_catalog_refresh() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("temporary-index-refresh.db");
    {
        let engine = Engine::open(&database).unwrap();
        let writer = engine.new_session().unwrap();
        engine
            .sql(
                "CREATE TEMP TABLE temp_items(id integer);
                 CREATE INDEX temp_items_idx ON temp_items(id)",
                &[],
            )
            .unwrap();
        writer
            .sql(
                "CREATE TABLE durable_items(id integer);
                 CREATE INDEX durable_items_idx ON durable_items(id)",
                &[],
            )
            .unwrap();

        let indexes = engine.list_catalog_indexes().unwrap();
        assert!(indexes.iter().any(|row| {
            row.relation.name == "temp_items_idx" && row.relation.schema.starts_with("pg_temp_")
        }));
        assert!(indexes
            .iter()
            .any(|row| row.relation == RelationIdentity::new("public", "durable_items_idx")));
        assert_eq!(
            engine
                .sql(
                    "SELECT to_regclass('temp_items_idx') IS NOT NULL AS present",
                    &[]
                )
                .unwrap()
                .rows[0]["present"],
            Value::Bool(true)
        );
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(index_identities(&reopened), ["public.durable_items_idx"]);
}

#[test]
fn quoted_index_identity_preserves_component_boundaries() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SCHEMA \"index.dot\";
             CREATE TABLE \"index.dot\".items(id integer);
             CREATE INDEX \"shared.dot\" ON \"index.dot\".items(id)",
            &[],
        )
        .unwrap();
    assert_eq!(index_identities(&engine), ["\"index.dot\".\"shared.dot\""]);
    let result = engine
        .sql(
            "SELECT '\"index.dot\".\"shared.dot\"'::regclass::oid =
                    to_regclass('\"index.dot\".\"shared.dot\"')::oid AS same_oid",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["same_oid"], Value::Bool(true));
    engine
        .sql("DROP INDEX \"index.dot\".\"shared.dot\"", &[])
        .unwrap();
    assert!(index_identities(&engine).is_empty());
}

#[test]
fn indexes_and_other_relations_cannot_share_one_schema_identity() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE indexed_items(id integer);
             CREATE TABLE occupied_name(id integer);
             CREATE INDEX occupied_index ON indexed_items(id)",
            &[],
        )
        .unwrap();

    for sql in [
        "CREATE INDEX occupied_index ON indexed_items(id)",
        "CREATE INDEX occupied_name ON indexed_items(id)",
        "CREATE TABLE occupied_index(id integer)",
    ] {
        let error = engine.sql(sql, &[]).expect_err(sql);
        assert_eq!(error.sqlstate(), Some("42P07"), "{sql}: {error}");
    }
    engine.take_sql_notices();
    engine
        .sql(
            "CREATE INDEX IF NOT EXISTS occupied_index ON indexed_items(id);
             CREATE INDEX IF NOT EXISTS occupied_name ON indexed_items(id)",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        [
            (
                "NOTICE".into(),
                "relation \"occupied_index\" already exists, skipping".into(),
            ),
            (
                "NOTICE".into(),
                "relation \"occupied_name\" already exists, skipping".into(),
            ),
        ]
    );
    assert_eq!(index_identities(&engine), ["public.occupied_index"]);
}

fn assert_unnamed_index_storage(db: &Path) {
    assert_eq!(catalog_index_count(db, "items"), 4);
    assert_eq!(storage_count(db, "_btree_indexes", "items", "qty"), 1);
    assert_eq!(storage_count(db, "_btree_index_entries", "items", "qty"), 3);
    assert!(storage_count(db, "_posting_clusters", "items", "body") > 0);
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
        assert_eq!(storage_count(&db, "_posting_clusters", "docs", "body"), 0);
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
                .map(|row| row.relation.qualified_name())
                .collect::<Vec<_>>(),
            vec!["public.docs_body_gin_b"]
        );
        assert_shared_gin_is_live(&engine);
        assert!(storage_count(&db, "_posting_clusters", "docs", "body") > 0);
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
        for storage_table in [
            "_posting_clusters",
            "_posting_documents",
            "_doc_lengths",
            "_field_stats",
        ] {
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
