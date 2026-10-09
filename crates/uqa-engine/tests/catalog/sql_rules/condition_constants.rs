//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;

fn assert_definition_and_execution(engine: &Engine, label: &str) {
    // PostgreSQL 18.4 independently supplies the definition and keeps the original sad row.
    assert_eq!(
        strings(
            engine,
            "SELECT pg_get_ruledef(oid) AS definition FROM pg_rewrite WHERE rulename='skip_happy'",
            "definition",
        ),
        [format!("CREATE RULE skip_happy AS\n    ON UPDATE TO public.feelings\n   WHERE (new.m = '{label}'::mood) DO INSTEAD NOTHING;")],
    );
    exec(engine, &format!("UPDATE feelings SET m='{label}'"));
    assert_eq!(
        strings(engine, "SELECT m::text AS m FROM feelings", "m"),
        ["sad"],
    );
}

#[test]
fn rule_enum_constants_follow_rename_rollback_and_persistent_reopen() {
    let directory = TempDir::new().unwrap();
    for provider in 0..4 {
        let path = directory
            .path()
            .join(format!("rule-constants-{provider}.db"));
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
        exec(&engine, "CREATE TYPE mood AS ENUM ('sad','happy'); CREATE TABLE feelings(id integer,m mood); CREATE RULE skip_happy AS ON UPDATE TO feelings WHERE NEW.m='happy' DO INSTEAD NOTHING; INSERT INTO feelings VALUES (1,'sad')");
        assert_definition_and_execution(&engine, "happy");
        exec(
            &engine,
            "BEGIN; SAVEPOINT kept; ALTER TYPE mood RENAME VALUE 'happy' TO 'glad'",
        );
        assert_definition_and_execution(&engine, "glad");
        exec(&engine, "ROLLBACK TO kept; COMMIT");
        assert_definition_and_execution(&engine, "happy");
        exec(&engine, "ALTER TYPE mood RENAME VALUE 'happy' TO 'glad'");
        assert_definition_and_execution(&engine, "glad");
        if provider != 0 {
            drop(engine);
            assert_definition_and_execution(&open(), "glad");
        }
    }
}
