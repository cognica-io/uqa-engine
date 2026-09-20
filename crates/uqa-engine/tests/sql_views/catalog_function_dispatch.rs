//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog function names must retain resolved routine identities and callback precedence.

use super::*;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
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
    }
}

fn assert_shadow(engine: &Engine, sql: &str) {
    let result = exec(engine, sql);
    assert_eq!(result.rows.len(), 2, "{sql}");
    for row in result.rows {
        assert_eq!(row["chosen"], Value::Str("shadow".into()), "{sql}");
    }
}

fn retained_catalog_function(provider: usize) {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("catalog-function.uqa");
    {
        let engine = open(provider, &path);
        exec(&engine, "CREATE SCHEMA shadow;
            CREATE FUNCTION shadow.pg_get_viewdef(oid) RETURNS text LANGUAGE SQL VOLATILE AS 'SELECT ''shadow''';
            CREATE TABLE public.references_to_views(id integer, ref oid);
            INSERT INTO public.references_to_views VALUES (1,0),(2,0);
            SET search_path=shadow,pg_catalog,public");
        let result = exec(
            &engine,
            "SELECT pg_get_viewdef(0::oid) AS chosen, pg_catalog.pg_get_viewdef(0::oid) AS builtin",
        );
        assert_eq!(result.rows[0]["chosen"], Value::Str("shadow".into()));
        assert_eq!(result.rows[0]["builtin"], Value::Null);
        for expression in [
            "pg_get_viewdef(ref)",
            "shadow.pg_get_viewdef(ref)",
            "coalesce(pg_get_viewdef(ref), 'missing')",
        ] {
            assert_shadow(
                &engine,
                &format!(
                    "SELECT {expression} AS chosen FROM public.references_to_views ORDER BY id"
                ),
            );
        }
        exec(&engine, "CREATE VIEW public.bound_catalog_function AS SELECT pg_get_viewdef(ref) AS chosen FROM public.references_to_views;
            SET search_path=pg_catalog,shadow,public");
        let result = exec(&engine, "SELECT pg_get_viewdef(0::oid) AS chosen");
        assert_eq!(result.rows[0]["chosen"], Value::Null);
        assert_shadow(&engine, "SELECT chosen FROM public.bound_catalog_function");
        exec(
            &engine,
            "ALTER FUNCTION shadow.pg_get_viewdef(oid) RENAME TO renamed_viewdef",
        );
        assert_shadow(&engine, "SELECT chosen FROM public.bound_catalog_function");
        exec(&engine, "BEGIN; SAVEPOINT before_rename; ALTER FUNCTION shadow.renamed_viewdef(oid) RENAME TO transient_viewdef");
        assert_shadow(&engine, "SELECT chosen FROM public.bound_catalog_function");
        exec(&engine, "ROLLBACK TO before_rename");
        assert_shadow(&engine, "SELECT chosen FROM public.bound_catalog_function");
        exec(&engine, "COMMIT");
    }
    let engine = open(provider, &path);
    exec(&engine, "SET search_path=pg_catalog,public");
    assert_shadow(&engine, "SELECT chosen FROM public.bound_catalog_function");
}

#[test]
fn catalog_named_routine_identity_survives_native_sqlite_reopen() {
    retained_catalog_function(0);
}

#[test]
fn catalog_named_routine_identity_survives_sqlite_keyvalue_reopen() {
    retained_catalog_function(1);
}

#[test]
fn catalog_named_routine_identity_survives_redb_reopen() {
    retained_catalog_function(2);
}

#[test]
fn catalog_named_callback_preserves_explicit_builtin_calls() {
    let engine = Engine::new();
    engine
        .register_scalar_function("pg_get_viewdef", |args: &[Value]| {
            assert_eq!(args, &[Value::Int(0)]);
            Ok(Value::Str("callback".into()))
        })
        .unwrap();
    let result = exec(
        &engine,
        "SELECT pg_get_viewdef(0::oid) AS chosen, pg_catalog.pg_get_viewdef(0::oid) AS builtin",
    );
    assert_eq!(result.rows[0]["chosen"], Value::Str("callback".into()));
    assert_eq!(result.rows[0]["builtin"], Value::Null);
    let error = engine
        .sql("SELECT pg_catalog.pg_get_viewdef(true)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
}
