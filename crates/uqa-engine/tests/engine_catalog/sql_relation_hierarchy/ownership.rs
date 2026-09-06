//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ownership of every descendant affected by a recursive schema change.

use super::{exec, Engine, Value};

fn setup(engine: &Engine, partitioned: bool) {
    for sql in [
        "CREATE ROLE recursive_parent_owner",
        "CREATE ROLE recursive_child_owner",
        "CREATE SCHEMA recursive_ownership",
        "GRANT USAGE, CREATE ON SCHEMA recursive_ownership TO recursive_parent_owner, recursive_child_owner",
    ] {
        exec(engine, sql);
    }
    if partitioned {
        exec(
            engine,
            "CREATE TABLE recursive_ownership.parent(id integer) PARTITION BY RANGE(id)",
        );
        exec(engine, "CREATE TABLE recursive_ownership.child PARTITION OF recursive_ownership.parent FOR VALUES FROM (0) TO (100)");
        exec(
            engine,
            "ALTER TABLE recursive_ownership.child ADD CONSTRAINT positive CHECK (id > 0)",
        );
    } else {
        exec(
            engine,
            "CREATE TABLE recursive_ownership.parent(id integer)",
        );
        exec(engine, "CREATE TABLE recursive_ownership.child(extra integer, mismatched text, later integer, CONSTRAINT positive CHECK (id > 0)) INHERITS (recursive_ownership.parent)");
    }
    for sql in [
        "ALTER TABLE recursive_ownership.child ALTER COLUMN id SET NOT NULL",
        "INSERT INTO recursive_ownership.child(id) VALUES (1)",
        "ALTER TABLE recursive_ownership.parent OWNER TO recursive_parent_owner",
        "ALTER TABLE recursive_ownership.child OWNER TO recursive_child_owner",
    ] {
        exec(engine, sql);
    }
}

fn catalog_snapshot(engine: &Engine) -> serde_json::Value {
    let attributes = engine.sql("SELECT c.relname, a.attname, a.attnotnull, a.attislocal, a.attinhcount FROM pg_catalog.pg_attribute AS a JOIN pg_catalog.pg_class AS c ON c.oid = a.attrelid WHERE c.relnamespace = 'recursive_ownership'::regnamespace AND a.attnum > 0 AND NOT a.attisdropped ORDER BY c.relname, a.attnum", &[]).unwrap();
    let constraints = engine.sql("SELECT c.relname, x.conname, x.contype, x.conislocal, x.coninhcount, x.convalidated FROM pg_catalog.pg_constraint AS x JOIN pg_catalog.pg_class AS c ON c.oid = x.conrelid WHERE c.relnamespace = 'recursive_ownership'::regnamespace ORDER BY c.relname, x.conname", &[]).unwrap();
    serde_json::json!({"attributes": attributes.rows, "constraints": constraints.rows})
}

fn deny_without_mutation(engine: &Engine, action: &str, relation: &str) {
    let before = catalog_snapshot(engine);
    exec(engine, "SET ROLE recursive_parent_owner");
    let sql = format!("ALTER TABLE recursive_ownership.parent {action}");
    let error = engine
        .sql(&sql, &[])
        .expect_err("descendant ownership is required");
    assert_eq!(error.sqlstate(), Some("42501"), "{sql}: {error}");
    assert_eq!(
        error.to_string(),
        format!("must be owner of table {relation}"),
        "{sql}"
    );
    exec(engine, "RESET ROLE");
    assert_eq!(catalog_snapshot(engine), before, "{sql}");
    let rows = engine
        .sql("SELECT id FROM recursive_ownership.parent ORDER BY id", &[])
        .unwrap();
    assert_eq!(rows.rows.len(), 1, "{sql}");
    assert_eq!(rows.rows[0]["id"], Value::Int(1), "{sql}");
}

#[test]
fn recursive_column_and_constraint_merges_require_each_child_owner() {
    let engine = Engine::new();
    setup(&engine, false);
    for action in [
        "ADD COLUMN fresh integer",
        "ADD COLUMN extra integer",
        "ADD COLUMN mismatched integer",
        "ADD CONSTRAINT positive CHECK (id > 0)",
        "ADD CONSTRAINT positive CHECK (id < 0) NOT VALID",
        "ADD CONSTRAINT required NOT NULL id",
        "ALTER COLUMN id SET NOT NULL",
    ] {
        deny_without_mutation(&engine, action, "child");
    }
    exec(
        &engine,
        "GRANT ALL ON recursive_ownership.child TO recursive_parent_owner",
    );
    deny_without_mutation(&engine, "ADD COLUMN extra integer", "child");
    exec(
        &engine,
        "GRANT recursive_child_owner TO recursive_parent_owner WITH INHERIT FALSE, SET TRUE",
    );
    deny_without_mutation(&engine, "ADD COLUMN extra integer", "child");
}

#[test]
fn partition_constraint_merges_require_the_partition_owner() {
    let engine = Engine::new();
    setup(&engine, true);
    for action in [
        "ADD CONSTRAINT positive CHECK (id > 0)",
        "ADD CONSTRAINT required NOT NULL id",
        "ALTER COLUMN id SET NOT NULL",
    ] {
        deny_without_mutation(&engine, action, "child");
    }
}

#[test]
fn merged_child_definitions_stop_recursion_before_unchanged_grandchildren() {
    let engine = Engine::new();
    setup(&engine, false);
    exec(
        &engine,
        "ALTER TABLE recursive_ownership.child OWNER TO recursive_parent_owner",
    );
    exec(
        &engine,
        "CREATE TABLE recursive_ownership.grandchild() INHERITS (recursive_ownership.child)",
    );
    exec(
        &engine,
        "ALTER TABLE recursive_ownership.grandchild OWNER TO recursive_child_owner",
    );
    let descendant = |engine: &Engine| {
        let mut snapshot = catalog_snapshot(engine);
        for rows in snapshot.as_object_mut().unwrap().values_mut() {
            rows.as_array_mut()
                .unwrap()
                .retain(|row| row["relname"] == "grandchild");
        }
        snapshot
    };
    let before = descendant(&engine);
    exec(&engine, "SET ROLE recursive_parent_owner");
    for sql in [
        "ALTER TABLE recursive_ownership.parent ADD COLUMN extra integer",
        "ALTER TABLE recursive_ownership.parent ADD CONSTRAINT positive CHECK (id > 0)",
        "ALTER TABLE recursive_ownership.parent ALTER COLUMN id SET NOT NULL",
    ] {
        exec(&engine, sql);
    }
    exec(&engine, "RESET ROLE");
    assert_eq!(descendant(&engine), before);
}

#[test]
fn new_child_definitions_reach_grandchild_ownership_checks_atomically() {
    let engine = Engine::new();
    setup(&engine, false);
    for sql in [
        "ALTER TABLE recursive_ownership.child DROP COLUMN extra",
        "ALTER TABLE recursive_ownership.child ALTER COLUMN id DROP NOT NULL",
        "ALTER TABLE recursive_ownership.child OWNER TO recursive_parent_owner",
        "CREATE TABLE recursive_ownership.grandchild(extra integer) INHERITS (recursive_ownership.child)",
        "ALTER TABLE recursive_ownership.grandchild ALTER COLUMN id SET NOT NULL",
        "ALTER TABLE recursive_ownership.grandchild OWNER TO recursive_child_owner",
    ] {
        exec(&engine, sql);
    }
    deny_without_mutation(&engine, "ADD COLUMN extra integer", "grandchild");
    deny_without_mutation(&engine, "ALTER COLUMN id SET NOT NULL", "grandchild");
}

#[test]
fn multiple_inheritance_follows_every_changing_parent_edge() {
    let engine = Engine::new();
    setup(&engine, false);
    for sql in [
        "ALTER TABLE recursive_ownership.child OWNER TO recursive_parent_owner",
        "CREATE TABLE recursive_ownership.other_child() INHERITS (recursive_ownership.parent)",
        "ALTER TABLE recursive_ownership.other_child OWNER TO recursive_parent_owner",
        "CREATE TABLE recursive_ownership.grandchild() INHERITS (recursive_ownership.child, recursive_ownership.other_child)",
        "ALTER TABLE recursive_ownership.grandchild OWNER TO recursive_child_owner",
    ] {
        exec(&engine, sql);
    }
    deny_without_mutation(&engine, "ADD COLUMN extra integer", "grandchild");
    deny_without_mutation(&engine, "ALTER COLUMN id SET NOT NULL", "grandchild");
    exec(
        &engine,
        "GRANT recursive_child_owner TO recursive_parent_owner WITH INHERIT TRUE, SET FALSE",
    );
    exec(&engine, "SET ROLE recursive_parent_owner");
    exec(
        &engine,
        "ALTER TABLE recursive_ownership.parent ADD COLUMN extra integer",
    );
    exec(&engine, "RESET ROLE");
    let inherited = engine.sql("SELECT attinhcount FROM pg_catalog.pg_attribute WHERE attrelid = 'recursive_ownership.grandchild'::regclass AND attname = 'extra'", &[]).unwrap();
    assert_eq!(inherited.rows[0]["attinhcount"], Value::Int(2));
}

#[test]
fn unchanged_root_definitions_do_not_require_child_ownership() {
    let engine = Engine::new();
    setup(&engine, false);
    exec(
        &engine,
        "ALTER TABLE recursive_ownership.parent ALTER COLUMN id SET NOT NULL",
    );
    let before = catalog_snapshot(&engine);
    exec(&engine, "SET ROLE recursive_parent_owner");
    for sql in [
        "ALTER TABLE recursive_ownership.parent ADD COLUMN IF NOT EXISTS id integer",
        "ALTER TABLE ONLY recursive_ownership.parent ADD COLUMN IF NOT EXISTS id text",
        "ALTER TABLE recursive_ownership.parent ALTER COLUMN id SET NOT NULL",
    ] {
        exec(&engine, sql);
    }
    exec(&engine, "RESET ROLE");
    assert_eq!(catalog_snapshot(&engine), before);
}

#[test]
fn inherited_child_ownership_allows_merges_and_revocation_takes_effect() {
    let engine = Engine::new();
    setup(&engine, false);
    exec(
        &engine,
        "GRANT recursive_child_owner TO recursive_parent_owner WITH INHERIT TRUE, SET FALSE",
    );
    exec(&engine, "SET ROLE recursive_parent_owner");
    for sql in [
        "ALTER TABLE recursive_ownership.parent ADD COLUMN extra integer",
        "ALTER TABLE recursive_ownership.parent ADD CONSTRAINT positive CHECK (id > 0)",
        "ALTER TABLE recursive_ownership.parent ALTER COLUMN id SET NOT NULL",
    ] {
        exec(&engine, sql);
    }
    exec(&engine, "RESET ROLE");
    let columns = engine.sql("SELECT attislocal, attinhcount FROM pg_catalog.pg_attribute WHERE attrelid = 'recursive_ownership.child'::regclass AND attname = 'extra'", &[]).unwrap();
    assert_eq!(columns.rows[0]["attislocal"], Value::Bool(true));
    assert_eq!(columns.rows[0]["attinhcount"], Value::Int(1));
    exec(
        &engine,
        "REVOKE recursive_child_owner FROM recursive_parent_owner",
    );
    deny_without_mutation(&engine, "ADD COLUMN later integer", "child");
}

#[test]
fn recursive_merge_ownership_and_savepoint_atomicity_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("recursive-ownership.db");
    let original;
    {
        let engine = Engine::open(&path).unwrap();
        setup(&engine, false);
        original = catalog_snapshot(&engine);
    }
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "SET ROLE recursive_parent_owner");
        exec(&engine, "BEGIN");
        exec(&engine, "SAVEPOINT protected_change");
        let error = engine
            .sql(
                "ALTER TABLE recursive_ownership.parent ADD COLUMN extra integer",
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"));
        assert_eq!(
            engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
            Some("25P02")
        );
        exec(&engine, "ROLLBACK TO protected_change");
        exec(&engine, "COMMIT");
        exec(&engine, "RESET ROLE");
        assert_eq!(catalog_snapshot(&engine), original);
    }
    {
        let engine = Engine::open(&path).unwrap();
        assert_eq!(catalog_snapshot(&engine), original);
        exec(
            &engine,
            "GRANT recursive_child_owner TO recursive_parent_owner WITH INHERIT TRUE, SET FALSE",
        );
    }
    let reopened = Engine::open(&path).unwrap();
    exec(&reopened, "SET ROLE recursive_parent_owner");
    exec(
        &reopened,
        "ALTER TABLE recursive_ownership.parent ADD COLUMN extra integer",
    );
    exec(&reopened, "RESET ROLE");
    assert_ne!(catalog_snapshot(&reopened), original);
}
