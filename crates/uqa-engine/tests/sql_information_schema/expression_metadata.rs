//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, Value};
use std::sync::Arc;

const RENAME: &str = "ALTER DOMAIN alias_types.positive RENAME TO renamed; ALTER FUNCTION alias_types.echo(regtype) RENAME TO changed";

fn defaults(engine: &Engine, qualified: bool, renamed: bool) {
    let schema = if qualified { "alias_types." } else { "" };
    let (routine, domain) = if renamed {
        ("changed", "renamed")
    } else {
        ("echo", "positive")
    };
    let result = engine.sql("SELECT column_name,column_default FROM information_schema.columns WHERE table_schema='public' AND table_name='alias_entries' ORDER BY column_name", &[]).unwrap();
    // PostgreSQL 18.4 returns these names for the same stored OIDs under each role/search path.
    let expected = [
        (
            "proc_ref",
            format!("'{schema}{routine}(regtype)'::regprocedure"),
        ),
        ("type_ref", format!("'{schema}{domain}'::regtype")),
    ];
    assert_eq!(result.rows.len(), expected.len());
    for (row, (name, default)) in result.rows.iter().zip(expected) {
        assert_eq!(row["column_name"], Value::Str(name.into()));
        assert_eq!(row["column_default"], Value::Str(default));
    }
}

#[test]
fn stored_alias_defaults_follow_visibility_rename_rollback_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    for provider in 0..4 {
        let path = directory
            .path()
            .join(format!("expression-output-{provider}.db"));
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
        engine.sql("CREATE ROLE alias_reader; CREATE SCHEMA alias_types; CREATE DOMAIN alias_types.positive AS integer DEFAULT abs(-7); CREATE FUNCTION alias_types.echo(n regtype DEFAULT 'alias_types.positive'::regtype) RETURNS regtype LANGUAGE SQL AS 'SELECT n'; CREATE TABLE public.alias_entries(type_ref regtype DEFAULT 'alias_types.positive'::regtype, proc_ref regprocedure DEFAULT 'alias_types.echo(regtype)'::regprocedure); GRANT SELECT ON public.alias_entries TO alias_reader",&[]).unwrap();
        defaults(&engine, true, false);
        engine
            .sql("SET search_path=alias_types,public", &[])
            .unwrap();
        defaults(&engine, false, false);
        engine.sql("SET ROLE alias_reader", &[]).unwrap();
        defaults(&engine, true, false);
        engine
            .sql("RESET ROLE; BEGIN; SAVEPOINT original", &[])
            .unwrap();
        engine.sql(RENAME, &[]).unwrap();
        defaults(&engine, false, true);
        engine.sql("ROLLBACK TO original; COMMIT", &[]).unwrap();
        defaults(&engine, false, false);
        engine.sql(RENAME, &[]).unwrap();
        defaults(&engine, false, true);
        if provider != 0 {
            drop(engine);
            let reopened = open();
            defaults(&reopened, true, true);
            reopened
                .sql("SET search_path=alias_types,public", &[])
                .unwrap();
            defaults(&reopened, false, true);
        }
    }
}
