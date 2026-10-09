//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, TempDir, Value};
use std::sync::Arc;

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0][&result.columns[0]].clone()
}

fn definition(name: &str, qualified: bool) -> Value {
    let schema = if qualified { "index_cache." } else { "" };
    Value::Str(format!(
        "CREATE INDEX {name} ON {schema}items USING btree (id) INCLUDE (note)"
    ))
}

#[test]
fn index_definition_lookups_keep_identity_visibility_rollback_and_reopen() {
    let directory = TempDir::new().unwrap();
    for provider in 0..4 {
        let path = directory
            .path()
            .join(format!("index-lookups-{provider}.db"));
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
        engine.sql("CREATE SCHEMA index_cache; CREATE ROLE index_reader; CREATE TABLE index_cache.items(id integer PRIMARY KEY, note text); CREATE INDEX selected_idx ON index_cache.items(id) INCLUDE(note)", &[]).unwrap();
        let Value::Int(oid) = scalar(&engine, "SELECT 'index_cache.selected_idx'::regclass::oid")
        else {
            panic!("index OID");
        };
        // PostgreSQL 18.4 independently supplies these full, pretty and selected-column definitions.
        assert_eq!(
            scalar(&engine, &format!("SELECT pg_get_indexdef({oid})")),
            definition("selected_idx", true)
        );
        assert_eq!(
            scalar(&engine, &format!("SELECT pg_get_indexdef({oid},1,true)")),
            Value::Str("id".into())
        );
        engine
            .sql("SET search_path=index_cache,public", &[])
            .unwrap();
        assert_eq!(
            scalar(&engine, &format!("SELECT pg_get_indexdef({oid},0,true)")),
            definition("selected_idx", false)
        );
        engine
            .sql(
                "BEGIN; SAVEPOINT kept; ALTER INDEX selected_idx RENAME TO renamed",
                &[],
            )
            .unwrap();
        assert_eq!(
            scalar(&engine, "SELECT 'renamed'::regclass::oid"),
            Value::Int(oid)
        );
        assert_eq!(
            scalar(&engine, &format!("SELECT pg_get_indexdef({oid})")),
            definition("renamed", true)
        );
        engine
            .sql("ROLLBACK TO kept; COMMIT; SET ROLE index_reader", &[])
            .unwrap();
        assert_eq!(
            scalar(&engine, &format!("SELECT pg_get_indexdef({oid},0,true)")),
            definition("selected_idx", true)
        );
        engine.sql("RESET ROLE", &[]).unwrap();
        if provider != 0 {
            drop(engine);
            let reopened = open();
            assert_eq!(
                scalar(
                    &reopened,
                    "SELECT 'index_cache.selected_idx'::regclass::oid"
                ),
                Value::Int(oid)
            );
            assert_eq!(
                scalar(&reopened, &format!("SELECT pg_get_indexdef({oid})")),
                definition("selected_idx", true)
            );
            reopened
                .sql("DROP INDEX index_cache.selected_idx", &[])
                .unwrap();
            assert_eq!(
                scalar(&reopened, &format!("SELECT pg_get_indexdef({oid})")),
                Value::Null
            );
        } else {
            engine
                .sql("DROP INDEX index_cache.selected_idx", &[])
                .unwrap();
            assert_eq!(
                scalar(&engine, &format!("SELECT pg_get_indexdef({oid})")),
                Value::Null
            );
        }
    }
}
