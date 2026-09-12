//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;

#[test]
fn creation_metadata_adapters_retain_actual_registry_and_session_guards() {
    let engine = Engine::new();
    let context = engine.relation_creation_context();
    let guards = [
        context.relations.tables(),
        context.relations.views(),
        context.relations.sequences(),
        context.relations.foreign_tables(),
        context.relations.indexes(),
    ];
    assert!(engine.storage.tables.try_write().is_none());
    assert!(engine.durable.views.is_locked());
    assert!(engine.durable.sequences.is_locked());
    assert!(engine.durable.foreign_tables.is_locked());
    assert!(engine.durable.catalog_indexes.is_locked());
    drop(guards);
    assert!(engine.storage.tables.try_write().is_some());
    assert!(!engine.durable.views.is_locked());
    assert!(!engine.durable.sequences.is_locked());
    assert!(!engine.durable.foreign_tables.is_locked());
    assert!(!engine.durable.catalog_indexes.is_locked());
    let path = context.state.search_path();
    let schemas = context.schemas.schemas();
    assert!(engine.session.state.try_write().is_none());
    assert!(engine.durable.schemas.is_locked());
    drop(schemas);
    drop(path);
    assert!(engine.session.state.try_write().is_some());
    assert!(!engine.durable.schemas.is_locked());
}

#[test]
fn sqlite_creation_preserves_ctas_collision_and_index_namespace_error_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("creation.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("CREATE ROLE reader; CREATE SCHEMA shared; CREATE SCHEMA private; CREATE TABLE shared.existing (id INTEGER); GRANT USAGE ON SCHEMA shared TO reader",&[]).unwrap();
    engine.sql("SET ROLE reader", &[]).unwrap();
    for (sql, code) in [
        ("CREATE TABLE shared.existing AS SELECT 1 AS id", "42P07"),
        ("CREATE TABLE shared.fresh AS SELECT 1 AS id", "42501"),
        ("CREATE INDEX denied_idx ON private.absent (id)", "42501"),
        ("CREATE INDEX missing_idx ON missing.absent (id)", "3F000"),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(code), "{sql}: {error}");
    }
    engine.sql("RESET ROLE", &[]).unwrap();
    assert!(engine.table("shared.fresh").unwrap().is_none());
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert!(reopened.table("shared.existing").unwrap().is_some());
    assert!(reopened.table("shared.fresh").unwrap().is_none());
}

#[test]
fn temporary_namespace_allocation_follows_validation_and_stays_session_local() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("temporary.db")).unwrap();
    assert!(!engine.temporary_namespace_allocated());
    assert!(engine
        .relation_creation_context()
        .temporary_name("public.docs")
        .is_err());
    assert!(!engine.temporary_namespace_allocated());
    assert_eq!(
        engine
            .relation_creation_context()
            .temporary_name("pg_temp.docs")
            .unwrap(),
        format!("{}.docs", engine.temporary_schema_name())
    );
    assert!(engine.temporary_namespace_allocated());
    let sibling = engine.new_session().unwrap();
    assert!(!sibling.temporary_namespace_allocated());
    assert_ne!(
        engine.temporary_schema_name(),
        sibling.temporary_schema_name()
    );
}
