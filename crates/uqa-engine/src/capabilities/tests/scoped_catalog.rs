//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime catalog readers reuse their selected inputs without freezing mutable session state.

use super::{query_scope, Engine, EngineHook, ScopedEngineHook, CATALOG_CAPTURES};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{ast::FunctionBinding, expr::composites::CompositeTypeCatalog};
use uqa_sql::{routines::resolution::RoutineOverloadCatalog, ColumnType};

fn lower_binding() -> FunctionBinding {
    FunctionBinding {
        name: "pg_catalog.lower".into(),
        argument_types: vec!["text".into()],
        builtin: true,
        object_id: None,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
    }
}

#[test]
fn scoped_permissions_and_overloads_reuse_catalog_inputs() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE DOMAIN amount AS integer; CREATE TABLE items (id amount)",
            &[],
        )
        .unwrap();
    let scope = query_scope::new_for_current_routine(&engine);
    let hook = ScopedEngineHook::new(&engine, &scope);
    let domains = scope.catalog_read_view().unwrap().domain_snapshot();
    let binding = lower_binding();
    CATALOG_CAPTURES.set(0);
    for _ in 0..16 {
        hook.require_builtin_execute(&binding).unwrap();
        let current = engine.routine_type_snapshot();
        assert!(Arc::ptr_eq(&domains, &current));
    }
    assert_eq!(CATALOG_CAPTURES.get(), 0);
}

#[rstest::rstest]
#[case::memory(false)]
#[case::sqlite(true)]
fn scalar_catalog_captures_do_not_grow_with_rows(#[case] persistent: bool) {
    let directory = tempfile::tempdir().unwrap();
    let engine = if persistent {
        Engine::open(&directory.path().join("catalog.db")).unwrap()
    } else {
        Engine::new()
    };
    engine.sql("CREATE TABLE items (id integer, note text); INSERT INTO items SELECT i, 'PAYLOAD' FROM generate_series(1, 9) AS g(i)", &[]).unwrap();
    for expression in [
        "lower(note)",
        "current_schema()",
        "current_schemas(false)",
        "to_regclass(note)",
        "to_regtype('integer')",
        "format_type(23, NULL)",
        "has_type_privilege('integer', 'USAGE')",
    ] {
        let captures = [1, 9].map(|rows| {
            engine.clear_regtype_output_cache();
            CATALOG_CAPTURES.set(0);
            let result = engine
                .sql(
                    &format!("SELECT {expression} AS value FROM items WHERE id <= {rows}"),
                    &[],
                )
                .unwrap();
            let count = CATALOG_CAPTURES.get();
            assert_eq!(result.rows.len(), rows);
            assert!(result
                .rows
                .iter()
                .all(|row| row["value"] == result.rows[0]["value"]));
            if expression == "lower(note)" {
                assert_eq!(result.rows[0]["value"], Value::Str("payload".into()));
            }
            count
        });
        assert!(
            captures[1] <= captures[0],
            "{expression}: captures grew with rows: {captures:?}"
        );
    }
}

#[rstest::rstest]
#[case::memory(false)]
#[case::sqlite(true)]
fn view_catalog_captures_do_not_grow_with_unrelated_views(#[case] persistent: bool) {
    let captures = [1, 4].map(|views| {
        let directory = tempfile::tempdir().unwrap();
        let engine = if persistent {
            Engine::open(&directory.path().join("catalog.db")).unwrap()
        } else {
            Engine::new()
        };
        engine.sql("CREATE TABLE items (id integer, note text, active boolean)", &[]).unwrap();
        for view in 0..views {
            engine.sql(&format!("CREATE VIEW item_view_{view} AS SELECT * FROM items"), &[]).unwrap();
        }
        CATALOG_CAPTURES.set(0);
        let result = engine.sql("SELECT column_name, is_updatable FROM information_schema.columns WHERE table_name = 'items' ORDER BY ordinal_position", &[]).unwrap();
        let count = CATALOG_CAPTURES.get();
        assert_eq!(result.rows.len(), 3);
        for (row, name) in result.rows.iter().zip(["id", "note", "active"]) {
            assert_eq!(row["column_name"], Value::Str(name.into()));
            assert_eq!(row["is_updatable"], Value::Str("YES".into()));
        }
        count
    });
    assert!(
        captures[1] <= captures[0],
        "captures grew with views: {captures:?}"
    );
}

#[test]
fn scoped_relation_descriptors_reuse_the_selected_generation() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE items (id integer); CREATE VIEW item_view AS SELECT id FROM items",
            &[],
        )
        .unwrap();
    let scope = query_scope::new_for_current_routine(&engine);
    let hook = ScopedEngineHook::new(&engine, &scope);
    for name in ["items", "item_view"] {
        let Some(ColumnType::Composite(ty)) = hook.resolve_type_name(name).unwrap() else {
            panic!("expected relation type");
        };
        CATALOG_CAPTURES.set(0);
        let descriptor = hook.composite_type(ty.oid).unwrap().unwrap();
        for _ in 0..16 {
            assert!(Arc::ptr_eq(
                &descriptor,
                &hook.composite_type(ty.oid).unwrap().unwrap()
            ));
            assert!(hook.composite_type(u32::MAX).unwrap().is_none());
        }
        assert_eq!(descriptor.attributes.len(), 1);
        assert_eq!(descriptor.attributes[0].name, "id");
        assert_eq!(CATALOG_CAPTURES.get(), 0);
    }
}

#[test]
fn scoped_runtime_catalog_observes_private_ddl_acl_and_rollback() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE reader; CREATE DOMAIN amount AS integer; CREATE TABLE items (id integer); SET search_path = pending, public", &[]).unwrap();
    let scope = query_scope::new_for_current_routine(&engine);
    let hook = ScopedEngineHook::new(&engine, &scope);
    let binding = lower_binding();
    let original = hook.resolve_regtype_input("amount").unwrap();
    assert_eq!(hook.current_schema().unwrap().as_deref(), Some("public"));
    engine.sql("BEGIN; CREATE SCHEMA pending; DROP DOMAIN amount; CREATE DOMAIN amount AS text; REVOKE EXECUTE ON FUNCTION lower(text) FROM PUBLIC; SET ROLE reader", &[]).unwrap();
    assert_eq!(
        hook.require_builtin_execute(&binding)
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_ne!(hook.resolve_regtype_input("amount").unwrap(), original);
    assert_eq!(hook.current_schema().unwrap().as_deref(), Some("pending"));
    engine.sql("ROLLBACK; SET ROLE reader", &[]).unwrap();
    hook.require_builtin_execute(&binding).unwrap();
    assert_eq!(hook.resolve_regtype_input("amount").unwrap(), original);
    assert_eq!(hook.current_schema().unwrap().as_deref(), Some("public"));
}

#[test]
fn scoped_runtime_catalog_keeps_the_portal_generation() {
    let mut engine = Engine::new();
    engine
        .sql(
            "CREATE DOMAIN amount AS integer; CREATE TABLE items (id integer)",
            &[],
        )
        .unwrap();
    let retained = Arc::new(crate::session::RetainedCatalogSnapshot {
        durable: engine.durable.snapshot(),
        read_view: engine.catalog_read_view(),
    });
    let domains = retained.read_view.domain_snapshot();
    let original = engine.resolve_regtype_input("amount").unwrap();
    engine.sql("DROP DOMAIN amount; CREATE DOMAIN amount AS text; ALTER TABLE items ADD COLUMN later text", &[]).unwrap();
    engine.query_catalog_snapshot = Some(retained);
    let scope = query_scope::new_for_current_routine(&engine);
    let hook = ScopedEngineHook::new(&engine, &scope);
    CATALOG_CAPTURES.set(0);
    assert_eq!(hook.resolve_regtype_input("amount").unwrap(), original);
    assert!(Arc::ptr_eq(&domains, &engine.routine_type_snapshot()));
    let Some(ColumnType::Composite(ty)) = hook.resolve_type_name("items").unwrap() else {
        panic!("expected relation type");
    };
    assert_eq!(
        hook.composite_type(ty.oid)
            .unwrap()
            .unwrap()
            .attributes
            .len(),
        1
    );
    assert_eq!(CATALOG_CAPTURES.get(), 0);
}

#[test]
fn scoped_catalog_scalars_see_nested_volatile_commands() {
    let engine = Engine::new();
    engine.sql("CREATE FUNCTION nested_catalog_change(i integer) RETURNS integer VOLATILE LANGUAGE plpgsql AS $$BEGIN IF i = 1 THEN EXECUTE 'CREATE SCHEMA scoped_callbacks_new'; PERFORM set_config('search_path', 'scoped_callbacks_new, public', false); END IF; RETURN i; END$$", &[]).unwrap();
    let result = engine.sql("SELECT nested_catalog_change(i) AS id, current_schema() AS schema FROM generate_series(1, 2) AS g(i)", &[]).unwrap();
    // PostgreSQL 18.4 in Docker returns the newly created schema for both rows.
    assert_eq!(result.rows.len(), 2);
    for (row, id) in result.rows.iter().zip([1, 2]) {
        assert_eq!(row["id"], Value::Int(id));
        assert_eq!(row["schema"], Value::Str("scoped_callbacks_new".into()));
    }
}

#[test]
fn runtime_catalog_does_not_mistake_an_older_scope_for_the_current_revision() {
    let engine = Engine::new();
    engine
        .sql("SET search_path = pending, public", &[])
        .unwrap();
    let scope = query_scope::new_for_current_routine(&engine);
    engine.sql("CREATE SCHEMA pending", &[]).unwrap();
    let hook = ScopedEngineHook::new(&engine, &scope);
    assert_eq!(hook.current_schema().unwrap().as_deref(), Some("pending"));
    CATALOG_CAPTURES.set(0);
    for _ in 0..16 {
        assert_eq!(hook.current_schema().unwrap().as_deref(), Some("pending"));
    }
    assert_eq!(CATALOG_CAPTURES.get(), 0);
}

#[test]
fn scoped_runtime_catalog_refreshes_peer_ddl_in_each_isolation_level() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("scoped.db");
            let engine = match provider {
                0 => Engine::open(&path).unwrap(),
                1 => Engine::from_persistent_provider(Arc::new(
                    uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
                ))
                .unwrap(),
                _ => Engine::from_persistent_provider(Arc::new(
                    uqa_storage_redb::RedbStorage::open(&path).unwrap(),
                ))
                .unwrap(),
            };
            engine
                .sql(
                    "CREATE TABLE items (id integer); INSERT INTO items VALUES (1)",
                    &[],
                )
                .unwrap();
            let peer = engine.new_session().unwrap();
            engine.sql(&format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM items; SET LOCAL search_path = pending, public"), &[]).unwrap();
            let scope = query_scope::new_for_current_routine(&engine);
            let hook = ScopedEngineHook::new(&engine, &scope);
            assert_eq!(hook.current_schema().unwrap().as_deref(), Some("public"));
            peer.sql("CREATE SCHEMA pending", &[]).unwrap();
            // PostgreSQL 18.4 observes peer DDL at the next SQL command in all three isolation levels.
            engine.sql("SELECT 1", &[]).unwrap();
            assert_eq!(
                hook.current_schema().unwrap().as_deref(),
                Some("pending"),
                "{provider}: {isolation}"
            );
            engine.sql("ROLLBACK", &[]).unwrap();
        }
    }
}
