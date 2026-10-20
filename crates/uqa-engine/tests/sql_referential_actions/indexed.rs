//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent SQL outcomes for indexed referential actions and NULL parent keys.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

fn open(provider: u8, path: &Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn indexed_foreign_keys_match_postgresql(#[case] provider: u8) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("indexed-references.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/fk_indexed_references_oracle.expected.json"
    ))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    drop(engine);
    if provider != 0 {
        let ids = reference["reopen_ids"].as_array().unwrap().clone();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| ids.contains(&case["id"]));
        crate::pg18_oracle::verify(&open(provider, &path), &reference.to_string());
    }
}

/// `PostgreSQL` 18.4 retains the trigger-suppressed child, rejects its missing parent at SET CONSTRAINTS, and accepts a replacement parent before the deferred check.
#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn deferred_root_deletes_preserve_trigger_selection_and_period_exclusions(#[case] provider: u8) {
    use uqa_core::Value;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("deferred-deletes.db");
    let engine = open(provider, &path);
    let exec = |sql: &str| engine.sql(sql, &[]).unwrap();
    exec("SET work_mem = '64kB'");
    for (table, create, insert, skip_id, repair, repaired_count) in [
        (
            "root_rows",
            "CREATE TABLE root_rows(id integer PRIMARY KEY, parent_id integer REFERENCES root_rows(id) DEFERRABLE INITIALLY DEFERRED)",
            "INSERT INTO root_rows VALUES (1,NULL),(2,1),(3,2)",
            3,
            "INSERT INTO root_rows VALUES (1,NULL),(2,1)",
            3,
        ),
        (
            "period_roots",
            "CREATE TABLE period_roots(id integer, valid int4range, parent_id integer, PRIMARY KEY (id,valid WITHOUT OVERLAPS), FOREIGN KEY (parent_id,PERIOD valid) REFERENCES period_roots(id,PERIOD valid) DEFERRABLE INITIALLY DEFERRED)",
            "INSERT INTO period_roots VALUES (1,'[0,2)',NULL),(1,'[2,4)',NULL),(2,'[1,3)',1)",
            2,
            "INSERT INTO period_roots VALUES (1,'[0,4)',NULL)",
            2,
        ),
    ] {
        exec(create);
        exec(insert);
        exec(&format!("CREATE FUNCTION keep_{table}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.id={skip_id} THEN RETURN NULL; END IF; RETURN OLD; END $$"));
        exec(&format!("CREATE TRIGGER keep_last BEFORE DELETE ON {table} FOR EACH ROW EXECUTE FUNCTION keep_{table}()"));
        exec("BEGIN; SAVEPOINT roots");
        assert_eq!(exec(&format!("DELETE FROM {table}")).affected_rows, 2);
        let remaining = exec(&format!("SELECT id FROM {table}"));
        assert_eq!(remaining.rows.len(), 1);
        assert_eq!(remaining.rows[0]["id"], Value::Int(skip_id));
        let error = engine.sql("SET CONSTRAINTS ALL IMMEDIATE", &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("23503"), "{table}: {error}");
        exec("ROLLBACK TO roots");
        assert_eq!(exec(&format!("DELETE FROM {table}")).affected_rows, 2);
        exec(repair);
        exec("SET CONSTRAINTS ALL IMMEDIATE; COMMIT");
        assert_eq!(exec(&format!("SELECT count(*) AS n FROM {table}")).rows[0]["n"], Value::Int(repaired_count));
        if table == "period_roots" {
            assert_eq!(exec("SELECT valid::text AS span FROM period_roots WHERE id=1").rows[0]["span"], Value::Str("[0,4)".into()));
        }
        exec(&format!("DROP TRIGGER keep_last ON {table}"));
        assert_eq!(exec(&format!("DELETE FROM {table}")).affected_rows, u64::try_from(repaired_count).unwrap());
        assert_eq!(exec(&format!("SELECT count(*) AS n FROM {table}")).rows[0]["n"], Value::Int(0));
    }
    drop(engine);
    if provider != 0 {
        let reopened = open(provider, &path);
        for table in ["root_rows", "period_roots"] {
            assert_eq!(
                reopened
                    .sql(&format!("SELECT count(*) AS n FROM {table}"), &[])
                    .unwrap()
                    .rows[0]["n"],
                Value::Int(0)
            );
        }
    }
}
