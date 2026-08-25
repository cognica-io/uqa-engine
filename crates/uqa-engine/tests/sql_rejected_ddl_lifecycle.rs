//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unsupported CREATE syntax must fail before it can create a durable object.

use uqa_engine::Engine;

#[test]
fn rejected_create_syntax_has_no_current_or_reopened_catalog_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("rejected_ddl.db");
    let rejected = [
        "CREATE TABLE inherited (id INTEGER) INHERITS (parent)",
        "CREATE TABLE optioned (id INTEGER) WITH (fillfactor = 70)",
        "CREATE SCHEMA owned AUTHORIZATION CURRENT_USER",
        "CREATE SCHEMA bundled CREATE TABLE bundled.child (id INTEGER)",
        "CREATE UNLOGGED VIEW unlogged_v AS SELECT 1",
        "CREATE TEMP MATERIALIZED VIEW temp_materialized AS SELECT 1",
    ];

    {
        let engine = Engine::open(&database).unwrap();
        for sql in rejected {
            assert!(
                engine.sql(sql, &[]).is_err(),
                "unsupported DDL succeeded: {sql}"
            );
        }
        assert!(!engine.has_schema("owned").unwrap());
        assert!(!engine.has_schema("bundled").unwrap());
        for relation in ["inherited", "optioned", "child", "temp_materialized"] {
            assert!(
                !engine.has_table(relation).unwrap(),
                "table leaked: {relation}"
            );
        }
        assert!(engine.view("unlogged_v").unwrap().is_none());
    }

    let reopened = Engine::open(&database).unwrap();
    assert!(!reopened.has_schema("owned").unwrap());
    assert!(!reopened.has_schema("bundled").unwrap());
    assert!(reopened.table_names().unwrap().is_empty());
    assert!(reopened.list_views().unwrap().is_empty());
    assert!(reopened.list_sequences().unwrap().is_empty());
}
