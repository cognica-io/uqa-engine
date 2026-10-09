//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, Value};
use std::{collections::BTreeMap, sync::Arc};

const RENAMES: &str =
    "ALTER TABLE parent RENAME TO renamed_parent; ALTER TABLE child RENAME COLUMN id TO item_id";

fn definitions(engine: &Engine, renamed: bool) -> BTreeMap<String, i64> {
    let result = engine.sql("SELECT conname, oid, pg_get_constraintdef(oid) AS definition, pg_get_constraintdef(oid,true) AS pretty FROM pg_constraint WHERE connamespace='constraint_inquiry'::regnamespace ORDER BY conname", &[]).unwrap();
    let column = if renamed { "item_id" } else { "id" };
    let parent = if renamed { "renamed_parent" } else { "parent" };
    // Definitions and pretty-print differences independently captured from PostgreSQL 18.4.
    let expected = [
        ("domain_positive", "CHECK ((VALUE > 0))".into()),
        ("domain_required", "NOT NULL".into()),
        (
            "parent_key",
            format!(
                "FOREIGN KEY (parent_id) REFERENCES {parent}(id) DEFERRABLE INITIALLY DEFERRED"
            ),
        ),
        ("parent_pk", "PRIMARY KEY (id)".into()),
        ("parent_required", "NOT NULL id".into()),
        ("positive", format!("CHECK (({column} > 0))")),
        ("required", format!("NOT NULL {column}")),
        ("unique_id", format!("UNIQUE ({column}) INCLUDE (extra)")),
    ];
    assert_eq!(result.rows.len(), expected.len());
    result
        .rows
        .into_iter()
        .zip(expected)
        .map(|(row, (name, definition))| {
            assert_eq!(row["conname"], Value::Str(name.into()));
            assert_eq!(row["definition"], Value::Str(definition.clone()));
            let pretty = definition
                .replace("CHECK ((", "CHECK (")
                .replace("0))", "0)");
            assert_eq!(row["pretty"], Value::Str(pretty));
            let Value::Int(oid) = row["oid"] else {
                panic!("constraint OID");
            };
            (name.into(), oid)
        })
        .collect()
}

fn drop_domain_check(engine: &Engine, oid: i64) {
    engine
        .sql("ALTER DOMAIN positive DROP CONSTRAINT domain_positive", &[])
        .unwrap();
    let result = engine
        .sql(
            &format!("SELECT pg_get_constraintdef({oid}) AS definition"),
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["definition"], Value::Null);
}

#[test]
fn constraint_definitions_keep_identity_rollback_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    for provider in 0..4 {
        let path = directory.path().join(format!("constraints-{provider}.db"));
        let open = || match provider {
            0 => Engine::new(),
            1 => Engine::open(&path).unwrap(),
            2 => Engine::from_persistent_provider(Arc::new(
                uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
            ))
            .unwrap(),
            _ => Engine::from_persistent_provider(Arc::new(
                uqa_storage_redb::RedbStorage::open(&path).unwrap(),
            ))
            .unwrap(),
        };
        let engine = open();
        engine.sql("CREATE SCHEMA constraint_inquiry; SET search_path=constraint_inquiry; CREATE TABLE parent(id integer CONSTRAINT parent_required NOT NULL, CONSTRAINT parent_pk PRIMARY KEY(id)); CREATE TABLE child(id integer CONSTRAINT required NOT NULL, parent_id integer, extra integer, CONSTRAINT positive CHECK(id > 0), CONSTRAINT unique_id UNIQUE(id) INCLUDE(extra), CONSTRAINT parent_key FOREIGN KEY(parent_id) REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE DOMAIN positive AS integer CONSTRAINT domain_required NOT NULL CONSTRAINT domain_positive CHECK(VALUE > 0)", &[]).unwrap();
        let original = definitions(&engine, false);
        engine.sql("BEGIN; SAVEPOINT kept", &[]).unwrap();
        engine.sql(RENAMES, &[]).unwrap();
        assert_eq!(definitions(&engine, true), original);
        engine.sql("ROLLBACK TO kept; COMMIT", &[]).unwrap();
        assert_eq!(definitions(&engine, false), original);
        engine.sql(RENAMES, &[]).unwrap();
        assert_eq!(definitions(&engine, true), original);
        if provider != 0 {
            drop(engine);
            let reopened = open();
            reopened
                .sql("SET search_path=constraint_inquiry", &[])
                .unwrap();
            assert_eq!(definitions(&reopened, true), original);
            drop_domain_check(&reopened, original["domain_positive"]);
        } else {
            drop_domain_check(&engine, original["domain_positive"]);
        }
    }
}
