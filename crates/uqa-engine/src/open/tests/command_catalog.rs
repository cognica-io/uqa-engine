//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog lock refresh retains the current command's private definitions until committed visibility changes.

use super::{Arc, Engine, Value};
use std::sync::atomic::Ordering;

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_kv(1)]
#[case::redb(2)]
fn catalog_lock_refresh_retains_own_definitions_and_observes_other_commits(
    #[case] provider: usize,
) {
    for unrelated in [1, 8] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("command-catalog.db");
        let open = || open_provider(provider, &path);
        let engine = open();
        for index in 0..unrelated {
            engine
                .sql(
                    &format!("CREATE TABLE unrelated_{index}(id int PRIMARY KEY)"),
                    &[],
                )
                .unwrap();
            assert_catalog_published(&engine);
        }
        engine.sql("SELECT 1", &[]).unwrap();
        let peer = engine.new_session().unwrap();
        pause_statistics(&peer);
        let previous = engine.storage.tables.read().clone();
        engine
            .sql("BEGIN; CREATE TABLE pending(id int PRIMARY KEY CHECK(id>0),v text DEFAULT 'private')", &[])
            .unwrap();
        assert_catalog_published(&engine);
        for _ in 0..3 {
            engine.refresh_explicit_statement_snapshot().unwrap();
            let current = engine.storage.tables.read();
            for (relation, table) in &previous {
                assert!(
                    Arc::ptr_eq(table, &current[relation]),
                    "unrelated table was rebuilt: {relation:?}"
                );
            }
        }

        peer.sql("CREATE TABLE published(id int PRIMARY KEY)", &[])
            .unwrap();
        assert_catalog_published(&peer);
        engine.refresh_explicit_statement_snapshot().unwrap();
        engine.sql("INSERT INTO pending(id) VALUES(1); INSERT INTO published VALUES(2); SAVEPOINT before_column; ALTER TABLE pending ADD COLUMN discarded int; ROLLBACK TO before_column", &[]).unwrap();
        let result = engine.sql("SELECT * FROM pending", &[]).unwrap();
        assert_eq!(result.rows[0]["v"], Value::Str("private".into()));
        assert!(!result.rows[0].contains_key("discarded"));
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(
            engine
                .sql("SELECT to_regclass('pending')", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Null)
        );
        assert_eq!(
            engine
                .sql("SELECT count(*) FROM published", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(0))
        );
        drop(previous);
        drop(peer);
        drop(engine);
        let reopened = open();
        assert_eq!(
            reopened
                .sql("SELECT count(*) FROM published", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(0))
        );
        assert_eq!(
            reopened
                .sql("SELECT to_regclass('pending')", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Null)
        );
    }
}

#[track_caller]
fn assert_catalog_published(engine: &Engine) {
    assert!(
        !uqa_execution::catalog::tuples::publication::has_pending(
            engine.storage.catalog.as_ref().unwrap().as_ref()
        )
        .unwrap(),
        "completed definitions must not leave publication work for another transaction"
    );
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_kv(1)]
#[case::redb(2)]
fn direct_schema_apis_finish_constraint_publication(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("direct-catalog.db");
    let engine = open_provider(provider, &path);
    engine
        .sql(
            "CREATE TABLE source(id int PRIMARY KEY CHECK(id>0), body text)",
            &[],
        )
        .unwrap();
    assert_catalog_published(&engine);
    engine
        .create_vector_field("source", "embedding", 2)
        .unwrap();
    assert_catalog_published(&engine);
    engine.add_fts_field("source", "body".into()).unwrap();
    assert_catalog_published(&engine);
    let uqa_sql::ast::Statement::CreateTable(mut declaration) =
        uqa_sql::compile("CREATE TABLE columns_to_register(extra int NOT NULL)")
            .unwrap()
            .remove(0)
    else {
        panic!("column declaration");
    };
    engine
        .register_column("source", declaration.columns.remove(0))
        .unwrap();
    assert_catalog_published(&engine);
    assert!(engine.rename_column("source", "id", "key").unwrap());
    assert_catalog_published(&engine);
    assert!(engine.rename_table("source", "destination").unwrap());
    assert_catalog_published(&engine);
    engine.truncate_table("destination").unwrap();
    assert_catalog_published(&engine);
    engine
        .sql(
            "CREATE TABLE indexed(id int PRIMARY KEY, body text, embedding vector(2))",
            &[],
        )
        .unwrap();
    for statement in [
        "CREATE INDEX indexed_body ON indexed USING gin(body)",
        "CREATE INDEX indexed_embedding ON indexed USING hnsw(embedding)",
        "DROP INDEX indexed_body",
        "DROP INDEX indexed_embedding",
    ] {
        engine.sql(statement, &[]).unwrap();
        assert_catalog_published(&engine);
    }
    engine
        .register_catalog_index("direct_body", "btree", "indexed", &["body".into()], &[])
        .unwrap();
    assert_catalog_published(&engine);
    assert!(engine.drop_catalog_index("direct_body").unwrap().is_some());
    assert_catalog_published(&engine);
    assert!(engine
        .set_column_default(
            "indexed",
            "body",
            Some(uqa_sql::ast::Expr::Literal(Value::Str("default".into())))
        )
        .unwrap());
    assert_catalog_published(&engine);
    assert!(engine.set_column_not_null("indexed", "body", true).unwrap());
    assert_catalog_published(&engine);
    assert!(engine
        .set_column_type(
            "indexed",
            "body",
            &uqa_sql::ast::ColumnType::Varchar(Some(32))
        )
        .unwrap());
    assert_catalog_published(&engine);
    let uqa_sql::ast::Statement::CreateTable(declaration) =
        uqa_sql::compile("CREATE TABLE constraints_to_register(id int, CHECK(id>0))")
            .unwrap()
            .remove(0)
    else {
        panic!("constraint declaration");
    };
    engine
        .register_table_constraints(
            "indexed",
            declaration.checks,
            declaration.foreign_keys,
            declaration.key_constraints,
        )
        .unwrap();
    assert_catalog_published(&engine);
    let definitions = "SELECT conname,pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='destination'::regclass ORDER BY conname";
    let expected = engine.sql(definitions, &[]).unwrap();
    assert_eq!(expected.rows.len(), 4);
    let indexed_definitions = "SELECT conname,pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='indexed'::regclass ORDER BY conname";
    let indexed_expected = engine.sql(indexed_definitions, &[]).unwrap();
    assert_eq!(indexed_expected.rows.len(), 4);
    engine.sql("BEGIN; SAVEPOINT before_rename", &[]).unwrap();
    assert!(engine
        .rename_column("destination", "key", "discarded")
        .unwrap());
    assert_catalog_published(&engine);
    engine
        .sql("ROLLBACK TO before_rename; COMMIT", &[])
        .unwrap();
    assert_eq!(engine.sql(definitions, &[]).unwrap().rows, expected.rows);
    drop(engine);
    let reopened = open_provider(provider, &path);
    assert_catalog_published(&reopened);
    assert_eq!(reopened.sql(definitions, &[]).unwrap().rows, expected.rows);
    assert_eq!(
        reopened.sql(indexed_definitions, &[]).unwrap().rows,
        indexed_expected.rows
    );
}

fn pause_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_kv(1)]
#[case::redb(2)]
fn own_ddl_commits_keep_stores_but_intervening_commits_refresh_them(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open_provider(provider, &directory.path().join("committed-catalog.db"));
    engine
        .sql(
            "CREATE TABLE unrelated(id int PRIMARY KEY); INSERT INTO unrelated VALUES(1); SELECT 1",
            &[],
        )
        .unwrap();
    let relation = uqa_core::RelationIdentity::new("public", "unrelated");
    let retained = Arc::clone(&engine.storage.tables.read()[&relation]);
    for statement in [
        "CREATE TABLE changed(id int PRIMARY KEY,v text CHECK(length(v)>0))",
        "CREATE INDEX changed_v ON changed(v)",
        "ALTER TABLE changed ADD COLUMN extra int DEFAULT 9",
        "CREATE VIEW visible AS SELECT * FROM changed",
        "CREATE FUNCTION owned_value() RETURNS int LANGUAGE SQL IMMUTABLE AS 'SELECT 7'",
    ] {
        engine.sql(statement, &[]).unwrap();
        assert_eq!(
            engine
                .sql("SELECT count(*) FROM unrelated", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(1))
        );
        assert!(
            Arc::ptr_eq(&retained, &engine.storage.tables.read()[&relation]),
            "own DDL commit rebuilt an unrelated store: {statement}"
        );
    }
    engine
        .sql("INSERT INTO changed(id,v) VALUES(1,'ready')", &[])
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT extra+owned_value() FROM visible", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(16))
    );

    let peer = engine.new_session().unwrap();
    pause_statistics(&peer);
    engine
        .sql("BEGIN; CREATE TABLE pending(id int PRIMARY KEY)", &[])
        .unwrap();
    peer.sql(
        "CREATE TABLE published(id int PRIMARY KEY); INSERT INTO published VALUES(2)",
        &[],
    )
    .unwrap();
    engine.sql("COMMIT", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM published", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(2))
    );
    assert_eq!(
        peer.sql("SELECT count(*) FROM pending", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(0))
    );
    assert!(
        !Arc::ptr_eq(&retained, &engine.storage.tables.read()[&relation]),
        "unobserved peer commits must retain the ordinary refresh"
    );
}

fn open_provider(provider: usize, path: &std::path::Path) -> Engine {
    let engine = match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    pause_statistics(&engine);
    engine
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_kv(1)]
#[case::redb(2)]
fn completed_ddl_batch_keeps_unrelated_stores_and_rolls_back_atomically(#[case] provider: usize) {
    for separate_messages in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("batch-catalog.db");
        let engine = open_provider(provider, &path);
        engine
            .sql(
                "CREATE TABLE unrelated(id int PRIMARY KEY); INSERT INTO unrelated VALUES(1)",
                &[],
            )
            .unwrap();
        let relation = uqa_core::RelationIdentity::new("public", "unrelated");
        let retained = Arc::clone(&engine.storage.tables.read()[&relation]);
        let statements = [
            "CREATE TABLE first(id int PRIMARY KEY,v int CHECK(v>0))",
            "CREATE INDEX first_v ON first(v)",
            "CREATE TABLE second(id int PRIMARY KEY,v int)",
            "CREATE INDEX second_v ON second(v)",
            "CREATE TABLE third(id int PRIMARY KEY,v int)",
            "CREATE INDEX third_v ON third(v)",
            "ALTER TABLE first ADD COLUMN extra int DEFAULT 9",
        ];
        if separate_messages {
            let batch: Vec<(&str, &[crate::SQLParam])> =
                statements.iter().map(|sql| (*sql, &[][..])).collect();
            engine.sql_batch_diagnosed(&batch).unwrap();
        } else {
            engine.sql(&statements.join(";"), &[]).unwrap();
        }
        engine
            .sql("INSERT INTO first(id,v) VALUES(1,2)", &[])
            .unwrap();
        assert_eq!(
            engine
                .sql("SELECT extra FROM first", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(9))
        );
        assert!(
            Arc::ptr_eq(&retained, &engine.storage.tables.read()[&relation]),
            "successful batch rebuilt an unrelated store"
        );
        engine.sql("BEGIN; SAVEPOINT before_column; ALTER TABLE first ADD COLUMN discarded int; ROLLBACK TO before_column", &[]).unwrap();
        assert!(!engine.sql("SELECT * FROM first", &[]).unwrap().rows[0].contains_key("discarded"));
        engine.sql("COMMIT", &[]).unwrap();
        assert!(engine.sql("CREATE TABLE failed(id int); CREATE INDEX failed_id ON failed(id); INSERT INTO first(id,v) VALUES(2,-1)", &[]).is_err());
        assert_eq!(
            engine
                .sql("SELECT to_regclass('failed')", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Null)
        );
        drop(retained);
        drop(engine);
        let reopened = open_provider(provider, &path);
        assert_eq!(
            reopened
                .sql("SELECT extra FROM first", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(9))
        );
        assert_eq!(
            reopened
                .sql("SELECT to_regclass('failed')", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Null)
        );
    }
}
