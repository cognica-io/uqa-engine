//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Databases written by releases that recorded user-defined type names in stored syntax and routine bindings open with those names bound to the types they meant, so a type renamed or moved afterwards keeps every stored reference working.

use std::path::Path;

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage_sqlite::ManagedConnection;

fn exec(engine: &Engine, sql: &str) -> uqa_sql::SQLResult {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = exec(engine, sql);
    result.rows[0][&result.columns[0]].clone()
}

fn type_oid(engine: &Engine, name: &str) -> i64 {
    let Value::Int(oid) = scalar(
        engine,
        &format!("SELECT oid::bigint FROM pg_type WHERE typname = '{name}'"),
    ) else {
        panic!("{name} has no OID");
    };
    oid
}

/// Spell every identity of `types` as the name earlier releases stored, in every record that holds stored syntax, and forget that the upgrade ran.
fn downgrade_to_names(database: &Path, types: &[(i64, &str)]) {
    let catalog =
        crate::native_storage::catalog(ManagedConnection::open(database).unwrap()).unwrap();
    let downgrade = |json: &str| {
        let mut json = json.to_string();
        for (oid, name) in types {
            json = json
                .replace(&format!("\"domain#{oid}[]\""), &format!("\"{name}[]\""))
                .replace(&format!("\"domain#{oid}\""), &format!("\"{name}\""));
        }
        json
    };
    for mut table in catalog.load_tables().unwrap() {
        table.columns_json = downgrade(&table.columns_json);
        table.constraints_json = downgrade(&table.constraints_json);
        catalog.save_table(&table).unwrap();
    }
    for mut view in catalog.load_views().unwrap() {
        view.definition_json = downgrade(&view.definition_json);
        catalog.save_view(&view).unwrap();
    }
    for mut index in catalog.load_catalog_indexes().unwrap() {
        index.columns_json = downgrade(&index.columns_json);
        index.definition_json = index.definition_json.as_deref().map(downgrade);
        catalog.save_catalog_index_row(&index).unwrap();
    }
    for key in ["sql_functions_json", "sql_triggers_json", "sql_rules_json"] {
        if let Some(json) = catalog.get_metadata(key).unwrap() {
            catalog.set_metadata(key, &downgrade(&json)).unwrap();
        }
    }
    for (key, json) in catalog.metadata_with_prefix("uqa.sql.domain.v1:").unwrap() {
        catalog.set_metadata(&key, &downgrade(&json)).unwrap();
    }
    catalog
        .delete_metadata("sql_stored_type_identity_version")
        .unwrap();
}

fn stored_json(database: &Path) -> String {
    let catalog =
        crate::native_storage::catalog(ManagedConnection::open(database).unwrap()).unwrap();
    let mut json = String::new();
    for table in catalog.load_tables().unwrap() {
        json.push_str(&table.columns_json);
        json.push_str(&table.constraints_json);
    }
    for view in catalog.load_views().unwrap() {
        json.push_str(&view.definition_json);
    }
    for index in catalog.load_catalog_indexes().unwrap() {
        json.push_str(&index.columns_json);
        json.push_str(index.definition_json.as_deref().unwrap_or_default());
    }
    for key in ["sql_functions_json", "sql_triggers_json", "sql_rules_json"] {
        json.push_str(&catalog.get_metadata(key).unwrap().unwrap_or_default());
    }
    json
}

fn verify(engine: &Engine) {
    assert_eq!(scalar(engine, "SELECT twice(21)"), Value::Int(42));
    assert_eq!(
        scalar(engine, "SELECT d FROM doubled WHERE id = 1"),
        Value::Int(12)
    );
    assert_eq!(
        scalar(engine, "SELECT s FROM doubled WHERE id = 1"),
        Value::Int(6)
    );
    exec(engine, "INSERT INTO amounts (id) VALUES (2)");
    assert_eq!(
        scalar(engine, "SELECT v FROM amounts WHERE id = 2"),
        Value::Int(6)
    );
    let error = engine
        .sql("INSERT INTO amounts (id, v) VALUES (3, 1)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"), "{error}");
    exec(engine, "DELETE FROM amounts WHERE id = 2");
    assert_eq!(scalar(engine, "SELECT guarded(4)"), Value::Int(8));
}

#[test]
fn stored_domain_names_from_earlier_releases_resolve_to_the_domains_they_meant() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("legacy-type-names.db");
    let types = {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE DOMAIN positive AS integer CHECK (VALUE > 0)",
            "CREATE DOMAIN small_positive AS positive CHECK (VALUE < 100)",
            "CREATE FUNCTION double_it(p positive) RETURNS integer LANGUAGE sql IMMUTABLE RETURN p * 2",
            "CREATE FUNCTION twice(p positive) RETURNS integer LANGUAGE sql BEGIN ATOMIC SELECT double_it(p); END",
            "CREATE FUNCTION guarded(p integer) RETURNS integer LANGUAGE sql BEGIN ATOMIC SELECT double_it(p::positive); END",
            "CREATE TABLE amounts (id integer, v integer DEFAULT double_it(3::positive), CHECK (v >= double_it(1::positive)))",
            "CREATE INDEX amounts_doubled ON amounts ((double_it(v::positive)))",
            "CREATE VIEW doubled AS SELECT id, double_it(v::positive) AS d, v::small_positive AS s FROM amounts",
            "INSERT INTO amounts (id) VALUES (1)",
        ] {
            exec(&engine, sql);
        }
        verify(&engine);
        [
            (type_oid(&engine, "positive"), "positive"),
            (type_oid(&engine, "small_positive"), "small_positive"),
        ]
    };
    downgrade_to_names(&database, &types);
    assert!(stored_json(&database).contains("\"positive\""));

    let engine = Engine::open(&database).unwrap();
    verify(&engine);
    exec(&engine, "ALTER DOMAIN positive RENAME TO strictly_positive");
    exec(&engine, "CREATE SCHEMA moved");
    exec(&engine, "ALTER DOMAIN small_positive SET SCHEMA moved");
    verify(&engine);
    drop(engine);

    let engine = Engine::open(&database).unwrap();
    verify(&engine);
    let stored = stored_json(&database);
    assert!(!stored.contains("\"positive\""), "{stored}");
    assert!(!stored.contains("\"small_positive\""), "{stored}");
}
