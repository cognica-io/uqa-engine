//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! End-to-end evidence for Engine capability boundaries.

use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

fn namespace_exists(engine: &Engine, name: &str) -> bool {
    !engine
        .sql(
            "SELECT nspname FROM pg_catalog.pg_namespace WHERE nspname = $1",
            &[SQLParam::Scalar(Value::Str(name.into()))],
        )
        .unwrap()
        .rows
        .is_empty()
}

#[test]
fn catalog_and_session_views_drive_virtual_catalog_rows() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA capability_read", &[]).unwrap();
    engine
        .sql("SET search_path TO capability_read, public", &[])
        .unwrap();
    engine.sql("SET work_mem TO '8MB'", &[]).unwrap();
    engine
        .sql("CREATE TEMP TABLE capability_temp (id INTEGER)", &[])
        .unwrap();

    assert!(namespace_exists(&engine, "capability_read"));
    let temporary = engine
        .sql(
            "SELECT nspname FROM pg_catalog.pg_namespace WHERE nspname LIKE 'pg_temp_%'",
            &[],
        )
        .unwrap();
    assert_eq!(temporary.rows.len(), 1);

    let settings = engine
        .sql(
            "SELECT name, setting FROM pg_catalog.pg_settings
             WHERE name IN ('search_path', 'work_mem') ORDER BY name",
            &[],
        )
        .unwrap();
    assert_eq!(settings.rows.len(), 2);
    assert_eq!(settings.rows[0]["name"], Value::Str("search_path".into()));
    assert_eq!(
        settings.rows[0]["setting"],
        Value::Str("capability_read,public".into())
    );
    assert_eq!(settings.rows[1]["name"], Value::Str("work_mem".into()));
    assert_eq!(settings.rows[1]["setting"], Value::Str("8MB".into()));
}

#[test]
fn schema_mutation_coordinator_preserves_rollback_commit_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("capability-schema.db");
    let root = Engine::open(&database).unwrap();
    let writer = root.new_session().unwrap();
    let observer = root.new_session().unwrap();

    writer
        .sql(
            "CREATE SCHEMA batch_schema_one; CREATE SCHEMA batch_schema_two",
            &[],
        )
        .unwrap();
    assert!(namespace_exists(&observer, "batch_schema_one"));
    assert!(namespace_exists(&observer, "batch_schema_two"));

    writer.begin().unwrap();
    writer.sql("CREATE SCHEMA rolled_back_schema", &[]).unwrap();
    assert!(namespace_exists(&writer, "rolled_back_schema"));
    assert!(!namespace_exists(&observer, "rolled_back_schema"));
    writer.rollback().unwrap();
    assert!(!namespace_exists(&writer, "rolled_back_schema"));

    writer.begin().unwrap();
    writer.sql("CREATE SCHEMA committed_schema", &[]).unwrap();
    assert!(!namespace_exists(&observer, "committed_schema"));
    writer.commit().unwrap();
    assert!(namespace_exists(&observer, "committed_schema"));

    drop(observer);
    drop(writer);
    drop(root);
    let reopened = Engine::open(&database).unwrap();
    assert!(namespace_exists(&reopened, "committed_schema"));
    assert!(!namespace_exists(&reopened, "rolled_back_schema"));
}
