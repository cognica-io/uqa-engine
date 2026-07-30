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
        "CREATE TEMP TABLE temp_t (id INTEGER)",
        "CREATE UNLOGGED TABLE unlogged_t (id INTEGER)",
        "CREATE TABLE inherited (id INTEGER) INHERITS (parent)",
        "CREATE TABLE optioned (id INTEGER) WITH (fillfactor = 70)",
        "CREATE SCHEMA owned AUTHORIZATION CURRENT_USER",
        "CREATE SCHEMA bundled CREATE TABLE bundled.child (id INTEGER)",
        "CREATE TEMP VIEW temp_v AS SELECT 1",
        "CREATE VIEW aliased(value) AS SELECT 1",
        "CREATE VIEW checked AS SELECT 1 WITH LOCAL CHECK OPTION",
        "CREATE MATERIALIZED VIEW materialized AS SELECT 1",
        "CREATE TEMP TABLE temp_as AS SELECT 1",
        "CREATE TABLE no_data AS SELECT 1 WITH NO DATA",
        "CREATE TEMP SEQUENCE temp_sequence",
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
        for relation in [
            "temp_t",
            "unlogged_t",
            "inherited",
            "optioned",
            "child",
            "materialized",
            "temp_as",
            "no_data",
        ] {
            assert!(
                !engine.has_table(relation).unwrap(),
                "table leaked: {relation}"
            );
        }
        for view in ["temp_v", "aliased", "checked"] {
            assert!(engine.view(view).unwrap().is_none(), "view leaked: {view}");
        }
        assert!(engine.sequence_state("temp_sequence").unwrap().is_none());
    }

    let reopened = Engine::open(&database).unwrap();
    assert!(!reopened.has_schema("owned").unwrap());
    assert!(!reopened.has_schema("bundled").unwrap());
    assert!(reopened.table_names().unwrap().is_empty());
    assert!(reopened.list_views().unwrap().is_empty());
    assert!(reopened.list_sequences().unwrap().is_empty());
}
