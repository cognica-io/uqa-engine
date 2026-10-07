//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Upgrade predecessor constructor syntax before ALTER TYPE changes the stored descriptor.

use serde_json::{json, Value as Json};
use std::path::Path;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::{CatalogFacade, PersistentStorageProvider};

fn catalog<T>(
    provider: usize,
    path: &Path,
    use_catalog: impl FnOnce(&dyn CatalogFacade) -> T,
) -> T {
    match provider {
        1 => use_catalog(
            &crate::native_storage::catalog(
                uqa_storage_sqlite::ManagedConnection::open(path).unwrap(),
            )
            .unwrap(),
        ),
        2 => {
            let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap();
            let session = storage.open_session().unwrap();
            use_catalog(session.catalog.as_ref())
        }
        3 => {
            let storage = uqa_storage_redb::RedbStorage::open(path).unwrap();
            let session = storage.open_session().unwrap();
            use_catalog(session.catalog.as_ref())
        }
        _ => unreachable!(),
    }
}

fn exec(engine: &Engine, sql: &str) -> uqa_sql::SQLResult {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

fn setup(engine: &Engine) {
    exec(engine, "CREATE TYPE saved_pair AS (a integer, b text);
        CREATE TABLE saved_rows (id integer, p saved_pair DEFAULT ROW(6,'default')::saved_pair,
            g integer GENERATED ALWAYS AS ((ROW(id,'generated')::saved_pair).a) STORED,
            CHECK ((ROW(id,'check')::saved_pair).a >= 0));
        CREATE VIEW saved_view AS SELECT ROW(1,'view')::saved_pair AS p;
        CREATE VIEW saved_dynamic AS SELECT ROW(id,'dynamic')::saved_pair AS p FROM saved_rows;
        CREATE FUNCTION saved_function() RETURNS saved_pair LANGUAGE SQL RETURN ROW(2,'function')::saved_pair;
        CREATE DOMAIN saved_domain AS saved_pair DEFAULT ROW(3,'domain')::saved_pair
            CHECK ((ROW((VALUE).a,'constraint')::saved_pair).a >= 0);
        CREATE TABLE saved_domains (p saved_domain);
        CREATE INDEX saved_index ON saved_rows (((ROW(id,'index')::saved_pair).a))
            WHERE (ROW(id,'predicate')::saved_pair).a > 0;
        CREATE TABLE saved_log (p saved_pair);
        CREATE RULE saved_rule AS ON INSERT TO saved_rows
            WHERE (ROW(NEW.id,'guard')::saved_pair).a > 0
            DO ALSO INSERT INTO saved_log VALUES (ROW(NEW.id,'rule')::saved_pair);
        INSERT INTO saved_rows (id) VALUES (4);
        CREATE TABLE saved_partitioned (id integer) PARTITION BY RANGE (((ROW(id,'key')::saved_pair).a));
        CREATE TABLE saved_partition PARTITION OF saved_partitioned FOR VALUES FROM (0) TO (10);
        INSERT INTO saved_partitioned VALUES (4)");
}

/// The predecessor AST represented the typed constructor as a cast of an anonymous ROW. Keep all other durable identities and catalog formats intact.
fn downgrade(value: &mut Json) -> usize {
    match value {
        Json::Array(values) => values.iter_mut().map(downgrade).sum(),
        Json::Object(object) => {
            let mut count = object.values_mut().map(downgrade).sum();
            if let Some(row) = object.remove("CompositeRow") {
                *value = json!({"Cast": {"expr": {"Row": row["items"]}, "ty": row["binding"]["ty"], "implicit": false}});
                count += 1;
            }
            count
        }
        _ => 0,
    }
}

fn downgrade_json(source: &str, count: &mut usize) -> String {
    let mut value: Json = serde_json::from_str(source).unwrap();
    *count += downgrade(&mut value);
    serde_json::to_string(&value).unwrap()
}

fn downgrade_catalog(catalog: &dyn CatalogFacade) {
    let mut count = 0;
    for mut table in catalog.load_tables().unwrap() {
        table.columns_json = downgrade_json(&table.columns_json, &mut count);
        table.constraints_json = downgrade_json(&table.constraints_json, &mut count);
        catalog.save_table(&table).unwrap();
    }
    for mut view in catalog.load_views().unwrap() {
        view.definition_json = downgrade_json(&view.definition_json, &mut count);
        catalog.save_view(&view).unwrap();
    }
    for mut index in catalog.load_catalog_indexes().unwrap() {
        index.columns_json = downgrade_json(&index.columns_json, &mut count);
        index.definition_json = index
            .definition_json
            .as_deref()
            .map(|json| downgrade_json(json, &mut count));
        catalog.save_catalog_index_row(&index).unwrap();
    }
    for key in ["sql_functions_json", "sql_rules_json", "sql_triggers_json"] {
        if let Some(json) = catalog.get_metadata(key).unwrap() {
            catalog
                .set_metadata(key, &downgrade_json(&json, &mut count))
                .unwrap();
        }
    }
    for (key, json) in catalog.metadata_with_prefix("uqa.sql.domain.v1:").unwrap() {
        catalog
            .set_metadata(&key, &downgrade_json(&json, &mut count))
            .unwrap();
    }
    assert!(
        count >= 10,
        "only {count} predecessor constructors were stored"
    );
}

fn verify(engine: &Engine) {
    let rows = exec(engine, "SELECT id FROM saved_partitioned ORDER BY id").rows;
    assert_eq!(
        rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
        [Value::Int(4), Value::Int(5)]
    );
    for (query, a, b) in [
        ("SELECT p FROM saved_view", 1, "view"),
        ("SELECT p FROM saved_dynamic WHERE (p).a = 4", 4, "dynamic"),
        ("SELECT saved_function() AS p", 2, "function"),
        ("SELECT p FROM saved_rows WHERE id = 4", 6, "default"),
        ("SELECT p FROM saved_domains", 3, "domain"),
        ("SELECT p FROM saved_log WHERE (p).a = 4 LIMIT 1", 4, "rule"),
    ] {
        let result = exec(engine, query);
        assert_eq!(
            result.rows[0]["p"],
            Value::Record(vec![
                ("a".into(), Value::Int(a)),
                ("b".into(), Value::Str(b.into())),
                ("c".into(), Value::Null)
            ]),
            "{query}"
        );
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn predecessor_constructors_are_migrated_before_attribute_addition(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constructor-migration.db");
    let engine = super::open(provider, &path);
    setup(&engine);
    drop(engine);
    catalog(provider, &path, downgrade_catalog);
    let engine = super::open(provider, &path);
    exec(
        &engine,
        "ALTER TYPE saved_pair ADD ATTRIBUTE c integer;
        INSERT INTO saved_rows (id) VALUES (5); INSERT INTO saved_domains DEFAULT VALUES;
        INSERT INTO saved_partitioned VALUES (5)",
    );
    verify(&engine);
    drop(engine);
    let engine = super::open(provider, &path);
    verify(&engine);
    exec(&engine, "INSERT INTO saved_rows (id) VALUES (6)");
}

fn catalog_snapshot(catalog: &dyn CatalogFacade) -> Vec<String> {
    let mut snapshot = catalog
        .load_tables()
        .unwrap()
        .iter()
        .map(|row| format!("{row:?}"))
        .collect::<Vec<_>>();
    snapshot.extend(
        catalog
            .load_views()
            .unwrap()
            .iter()
            .map(|row| format!("{row:?}")),
    );
    snapshot.extend(
        catalog
            .load_catalog_indexes()
            .unwrap()
            .iter()
            .map(|row| format!("{row:?}")),
    );
    snapshot.extend(
        catalog
            .metadata_with_prefix("")
            .unwrap()
            .iter()
            .map(|row| format!("{row:?}")),
    );
    snapshot.sort();
    snapshot
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn failed_constructor_migration_preserves_the_predecessor_catalog(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constructor-rollback.db");
    let engine = super::open(provider, &path);
    setup(&engine);
    drop(engine);
    let (rules, before) = catalog(provider, &path, |catalog| {
        downgrade_catalog(catalog);
        let rules = catalog.get_metadata("sql_rules_json").unwrap().unwrap();
        let mut invalid: Json = serde_json::from_str(&rules).unwrap();
        invalid["format_version"] = json!(u32::MAX);
        catalog
            .set_metadata("sql_rules_json", &invalid.to_string())
            .unwrap();
        (rules, catalog_snapshot(catalog))
    });
    let result = match provider {
        1 => Engine::open(&path).map_err(|error| error.to_string()),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .map_err(|error| error.to_string()),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .map_err(|error| error.to_string()),
        _ => unreachable!(),
    };
    let Err(error) = result else {
        panic!("future rule format must reject initial restoration")
    };
    assert!(error.contains("newer than supported"), "{error}");
    catalog(provider, &path, |catalog| {
        assert_eq!(catalog_snapshot(catalog), before);
        catalog.set_metadata("sql_rules_json", &rules).unwrap();
    });
    let engine = super::open(provider, &path);
    exec(
        &engine,
        "ALTER TYPE saved_pair ADD ATTRIBUTE c integer;
        INSERT INTO saved_domains DEFAULT VALUES; INSERT INTO saved_partitioned VALUES (5)",
    );
    verify(&engine);
}
