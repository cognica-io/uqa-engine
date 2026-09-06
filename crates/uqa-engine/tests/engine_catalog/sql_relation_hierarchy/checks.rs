//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` CHECK inheritance, local origin, and recursive lifecycle.

use super::{exec, Engine, Value};

fn checks(engine: &Engine, table: &str) -> Vec<(String, bool, i64, bool)> {
    engine.sql(&format!("SELECT conname, conislocal, coninhcount, convalidated FROM pg_constraint WHERE conrelid='{table}'::regclass AND contype='c' ORDER BY conname"), &[]).unwrap().rows.into_iter().map(|row| {
        let Value::Str(name) = &row["conname"] else { panic!("constraint name is not text") };
        let Value::Bool(local) = row["conislocal"] else { panic!("local origin is not boolean") };
        let Value::Int(parents) = row["coninhcount"] else { panic!("inheritance count is not integer") };
        let Value::Bool(validated) = row["convalidated"] else { panic!("validation state is not boolean") };
        (name.clone(), local, parents, validated)
    }).collect()
}

fn state(engine: &Engine, table: &str, name: &str, local: bool, parents: i64, validated: bool) {
    assert_eq!(
        checks(engine, table),
        vec![(name.into(), local, parents, validated)],
        "{table}"
    );
}

fn error(engine: &Engine, sql: &str, sqlstate: &str) {
    assert_eq!(
        engine.sql(sql, &[]).unwrap_err().sqlstate(),
        Some(sqlstate),
        "{sql}"
    );
}

#[test]
fn inherited_and_local_column_and_table_checks_merge_by_definition_and_keep_origin() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_parent(a integer CONSTRAINT positive CHECK(a>0))",
    );
    exec(
        &engine,
        "CREATE TABLE check_inherited() INHERITS(check_parent)",
    );
    state(&engine, "check_inherited", "positive", false, 1, true);
    for (name, declaration) in [
        ("check_column", "a integer CONSTRAINT positive CHECK(a>0)"),
        ("check_table", "a integer, CONSTRAINT positive CHECK(a>0)"),
    ] {
        exec(
            &engine,
            &format!("CREATE TABLE {name}({declaration}) INHERITS(check_parent)"),
        );
        state(&engine, name, "positive", true, 1, true);
        error(&engine, &format!("INSERT INTO {name} VALUES(-1)"), "23514");
    }
    error(
        &engine,
        "CREATE TABLE check_bad(a integer CONSTRAINT positive CHECK(a>=0)) INHERITS(check_parent)",
        "42710",
    );
    error(&engine, "CREATE TABLE check_noinherit(a integer CONSTRAINT positive CHECK(a>0) NO INHERIT) INHERITS(check_parent)", "42P17");
}

#[test]
fn recursive_add_preserves_local_checks_and_rejects_unvalidated_child_conflicts_atomically() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE check_parent(a integer)");
    exec(
        &engine,
        "CREATE TABLE check_local(a integer CONSTRAINT positive CHECK(a>0)) INHERITS(check_parent)",
    );
    exec(
        &engine,
        "CREATE TABLE check_inherited() INHERITS(check_parent)",
    );
    exec(
        &engine,
        "ALTER TABLE check_parent ADD CONSTRAINT positive CHECK(a>0)",
    );
    state(&engine, "check_parent", "positive", true, 0, true);
    state(&engine, "check_local", "positive", true, 1, true);
    state(&engine, "check_inherited", "positive", false, 1, true);
    exec(&engine, "CREATE TABLE invalid_parent(a integer)");
    exec(
        &engine,
        "CREATE TABLE invalid_child() INHERITS(invalid_parent)",
    );
    exec(&engine, "INSERT INTO invalid_child VALUES(-1)");
    exec(
        &engine,
        "ALTER TABLE invalid_child ADD CONSTRAINT positive CHECK(a>0) NOT VALID",
    );
    error(
        &engine,
        "ALTER TABLE invalid_parent ADD CONSTRAINT positive CHECK(a>0)",
        "42P17",
    );
    assert!(checks(&engine, "invalid_parent").is_empty());
    state(&engine, "invalid_child", "positive", true, 0, false);
}

#[test]
fn adding_an_inherited_check_locally_preserves_identity_and_requires_matching_validation() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_parent(a integer, CONSTRAINT positive CHECK(a>0))",
    );
    exec(&engine, "CREATE TABLE check_child() INHERITS(check_parent)");
    let identity = engine
        .sql(
            "SELECT oid FROM pg_constraint WHERE conrelid='check_child'::regclass",
            &[],
        )
        .unwrap()
        .rows;
    exec(
        &engine,
        "ALTER TABLE check_child ADD CONSTRAINT positive CHECK(a>0)",
    );
    state(&engine, "check_child", "positive", true, 1, true);
    assert_eq!(
        engine
            .sql(
                "SELECT oid FROM pg_constraint WHERE conrelid='check_child'::regclass",
                &[]
            )
            .unwrap()
            .rows,
        identity
    );
    error(
        &engine,
        "ALTER TABLE check_child ADD CONSTRAINT positive CHECK(a>0)",
        "42710",
    );
    exec(&engine, "CREATE TABLE invalid_parent(a integer)");
    exec(
        &engine,
        "CREATE TABLE invalid_child() INHERITS(invalid_parent)",
    );
    exec(&engine, "INSERT INTO invalid_child VALUES(-1)");
    exec(
        &engine,
        "ALTER TABLE invalid_parent ADD CONSTRAINT positive CHECK(a>0) NOT VALID",
    );
    error(
        &engine,
        "ALTER TABLE invalid_child ADD CONSTRAINT positive CHECK(a>0)",
        "42P17",
    );
    state(&engine, "invalid_child", "positive", false, 1, false);
}

#[test]
fn recursive_and_only_drop_preserve_local_or_still_inherited_checks() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_left(a integer, CONSTRAINT positive CHECK(a>0))",
    );
    exec(
        &engine,
        "CREATE TABLE check_right(a integer, CONSTRAINT positive CHECK(a>0))",
    );
    exec(
        &engine,
        "CREATE TABLE check_shared() INHERITS(check_left,check_right)",
    );
    exec(
        &engine,
        "CREATE TABLE check_local(a integer, CONSTRAINT positive CHECK(a>0)) INHERITS(check_left)",
    );
    error(
        &engine,
        "ALTER TABLE check_shared DROP CONSTRAINT positive",
        "42P16",
    );
    error(
        &engine,
        "ALTER TABLE check_local DROP CONSTRAINT positive",
        "42P16",
    );
    exec(&engine, "ALTER TABLE check_left DROP CONSTRAINT positive");
    state(&engine, "check_shared", "positive", false, 1, true);
    state(&engine, "check_local", "positive", true, 0, true);
    exec(&engine, "ALTER TABLE check_right DROP CONSTRAINT positive");
    assert!(checks(&engine, "check_shared").is_empty());
    exec(
        &engine,
        "CREATE TABLE only_parent(a integer, CONSTRAINT positive CHECK(a>0))",
    );
    exec(&engine, "CREATE TABLE only_child() INHERITS(only_parent)");
    exec(
        &engine,
        "ALTER TABLE ONLY only_parent DROP CONSTRAINT positive",
    );
    state(&engine, "only_child", "positive", true, 0, true);
}

#[test]
fn constraint_rename_requires_all_supplying_parents_and_updates_descendants_atomically() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_parent(a integer, CONSTRAINT positive CHECK(a>0))",
    );
    exec(&engine, "CREATE TABLE check_child() INHERITS(check_parent)");
    let identities = engine.sql("SELECT oid FROM pg_constraint WHERE conrelid IN('check_parent'::regclass,'check_child'::regclass) ORDER BY conrelid", &[]).unwrap().rows;
    error(
        &engine,
        "ALTER TABLE check_child RENAME CONSTRAINT positive TO renamed",
        "42P16",
    );
    error(
        &engine,
        "ALTER TABLE ONLY check_parent RENAME CONSTRAINT positive TO renamed",
        "42P16",
    );
    exec(
        &engine,
        "ALTER TABLE check_parent RENAME CONSTRAINT positive TO renamed",
    );
    state(&engine, "check_child", "renamed", false, 1, true);
    assert_eq!(engine.sql("SELECT oid FROM pg_constraint WHERE conrelid IN('check_parent'::regclass,'check_child'::regclass) ORDER BY conrelid", &[]).unwrap().rows, identities);
    exec(
        &engine,
        "CREATE TABLE check_second(a integer, CONSTRAINT renamed CHECK(a>0))",
    );
    exec(&engine, "ALTER TABLE check_child INHERIT check_second");
    error(
        &engine,
        "ALTER TABLE check_parent RENAME CONSTRAINT renamed TO forbidden",
        "42P16",
    );
    state(&engine, "check_parent", "renamed", true, 0, true);
    state(&engine, "check_child", "renamed", false, 2, true);
}

#[test]
fn inheritance_removal_and_partition_origins_survive_savepoints_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("check-inheritance.db");
    let engine = Engine::open(&path).unwrap();
    exec(
        &engine,
        "CREATE TABLE check_parent(a integer, CONSTRAINT positive CHECK(a>0))",
    );
    exec(&engine, "CREATE TABLE check_child() INHERITS(check_parent)");
    exec(&engine, "BEGIN");
    exec(&engine, "SAVEPOINT edge_change");
    exec(&engine, "ALTER TABLE check_child NO INHERIT check_parent");
    state(&engine, "check_child", "positive", true, 0, true);
    exec(&engine, "ROLLBACK TO edge_change");
    state(&engine, "check_child", "positive", false, 1, true);
    exec(&engine, "COMMIT");
    exec(&engine, "CREATE TABLE check_partitioned(a integer, CONSTRAINT positive CHECK(a>0)) PARTITION BY RANGE(a)");
    exec(
        &engine,
        "CREATE TABLE check_born PARTITION OF check_partitioned FOR VALUES FROM(0) TO(10)",
    );
    exec(
        &engine,
        "CREATE TABLE check_attached(a integer, CONSTRAINT positive CHECK(a>0))",
    );
    exec(
        &engine,
        "ALTER TABLE check_partitioned ATTACH PARTITION check_attached FOR VALUES FROM(10) TO(20)",
    );
    state(&engine, "check_born", "positive", false, 1, true);
    state(&engine, "check_attached", "positive", false, 1, true);
    exec(
        &engine,
        "ALTER TABLE check_partitioned DETACH PARTITION check_born",
    );
    drop(engine);
    let engine = Engine::open(&path).unwrap();
    state(&engine, "check_child", "positive", false, 1, true);
    state(&engine, "check_born", "positive", true, 0, true);
    state(&engine, "check_attached", "positive", false, 1, true);
}

#[test]
fn no_inherit_column_and_table_checks_are_excluded_from_new_children() {
    for declaration in [
        "a integer CONSTRAINT positive CHECK(a>0) NO INHERIT",
        "a integer, CONSTRAINT positive CHECK(a>0) NO INHERIT",
    ] {
        let engine = Engine::new();
        exec(
            &engine,
            &format!("CREATE TABLE check_parent({declaration})"),
        );
        exec(&engine, "CREATE TABLE check_child() INHERITS(check_parent)");
        exec(&engine, "INSERT INTO check_child VALUES(-1)");
        assert!(checks(&engine, "check_child").is_empty());
    }
}

#[test]
fn check_validation_requires_descendants_and_rolls_back_the_complete_change() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE check_parent(a integer)");
    exec(&engine, "CREATE TABLE check_child() INHERITS(check_parent)");
    exec(&engine, "INSERT INTO check_child VALUES(-1)");
    exec(
        &engine,
        "ALTER TABLE check_parent ADD CONSTRAINT positive CHECK(a>0) NOT VALID",
    );
    exec(&engine, "CREATE TABLE check_born() INHERITS(check_parent)");
    state(&engine, "check_born", "positive", false, 1, true);
    error(
        &engine,
        "ALTER TABLE check_parent VALIDATE CONSTRAINT positive",
        "23514",
    );
    state(&engine, "check_parent", "positive", true, 0, false);
    state(&engine, "check_child", "positive", false, 1, false);
    exec(&engine, "UPDATE check_child SET a=1");
    error(
        &engine,
        "ALTER TABLE ONLY check_parent VALIDATE CONSTRAINT positive",
        "42P16",
    );
    exec(
        &engine,
        "ALTER TABLE check_parent VALIDATE CONSTRAINT positive",
    );
    state(&engine, "check_parent", "positive", true, 0, true);
    state(&engine, "check_child", "positive", false, 1, true);
}

#[test]
fn a_diamond_renames_each_constraint_once_and_drops_each_inheritance_edge() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_root(a integer CONSTRAINT positive CHECK(check_root.a>0))",
    );
    exec(&engine, "CREATE TABLE check_left() INHERITS(check_root)");
    exec(&engine, "CREATE TABLE check_right() INHERITS(check_root)");
    exec(
        &engine,
        "CREATE TABLE check_leaf() INHERITS(check_left,check_right)",
    );
    state(&engine, "check_leaf", "positive", false, 2, true);
    exec(
        &engine,
        "ALTER TABLE check_root RENAME CONSTRAINT positive TO renamed",
    );
    state(&engine, "check_leaf", "renamed", false, 2, true);
    exec(&engine, "ALTER TABLE check_root DROP CONSTRAINT renamed");
    for table in ["check_root", "check_left", "check_right", "check_leaf"] {
        assert!(checks(&engine, table).is_empty(), "{table}");
    }
}

#[test]
fn recursive_check_drop_and_rename_check_descendant_owners_before_publishing() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE check_parent_owner",
        "CREATE ROLE check_child_owner",
        "CREATE SCHEMA check_owners",
        "GRANT USAGE ON SCHEMA check_owners TO check_parent_owner,check_child_owner",
        "CREATE TABLE check_owners.parent(a integer CONSTRAINT positive CHECK(a>0))",
        "CREATE TABLE check_owners.child() INHERITS(check_owners.parent)",
        "ALTER TABLE check_owners.parent OWNER TO check_parent_owner",
        "ALTER TABLE check_owners.child OWNER TO check_child_owner",
    ] {
        exec(&engine, sql);
    }
    for action in [
        "DROP CONSTRAINT positive",
        "RENAME CONSTRAINT positive TO renamed",
    ] {
        exec(&engine, "SET ROLE check_parent_owner");
        error(
            &engine,
            &format!("ALTER TABLE check_owners.parent {action}"),
            "42501",
        );
        exec(&engine, "RESET ROLE");
        state(&engine, "check_owners.parent", "positive", true, 0, true);
        state(&engine, "check_owners.child", "positive", false, 1, true);
    }
    exec(
        &engine,
        "CREATE TABLE check_owners.ancestor(a integer CONSTRAINT positive CHECK(a>0))",
    );
    exec(
        &engine,
        "ALTER TABLE check_owners.parent INHERIT check_owners.ancestor",
    );
    exec(&engine, "SET ROLE check_parent_owner");
    error(
        &engine,
        "ALTER TABLE check_owners.parent RENAME CONSTRAINT positive TO renamed",
        "42501",
    );
    exec(&engine, "RESET ROLE");
    state(&engine, "check_owners.parent", "positive", true, 1, true);
    state(&engine, "check_owners.child", "positive", false, 1, true);
}

#[test]
fn column_merge_keeps_check_propagation_independent_and_leaves_legacy_rows_unvalidated() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE legacy(a integer)");
    exec(&engine, "INSERT INTO legacy VALUES(-1)");
    exec(
        &engine,
        "ALTER TABLE legacy ADD CONSTRAINT legacy_positive CHECK(a>0) NOT VALID",
    );
    exec(&engine, "CREATE TABLE check_parent(a integer)");
    exec(&engine, "CREATE TABLE check_child(b integer CONSTRAINT child_positive CHECK(b>0)) INHERITS(check_parent)");
    exec(
        &engine,
        "CREATE TABLE check_grandchild() INHERITS(check_child)",
    );
    exec(
        &engine,
        "ALTER TABLE check_parent ADD COLUMN b integer CONSTRAINT parent_positive CHECK(b>=0)",
    );
    assert_eq!(
        checks(&engine, "check_child"),
        vec![
            ("child_positive".into(), true, 0, true),
            ("parent_positive".into(), false, 1, true)
        ]
    );
    assert_eq!(
        checks(&engine, "check_grandchild"),
        vec![
            ("child_positive".into(), false, 1, true),
            ("parent_positive".into(), false, 1, true)
        ]
    );
    state(&engine, "legacy", "legacy_positive", true, 0, false);
    exec(&engine, "ALTER TABLE check_parent ADD COLUMN c integer CONSTRAINT local_positive CHECK(c>0) NO INHERIT");
    exec(&engine, "INSERT INTO check_child VALUES(1,1,-1)");
    exec(&engine, "ALTER TABLE check_parent ADD COLUMN IF NOT EXISTS b integer CONSTRAINT skipped CHECK(b>10)");
    assert_eq!(checks(&engine, "check_parent").len(), 2);
}

#[test]
fn check_merges_use_bound_columns_and_constant_types_and_reject_duplicate_local_names() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE check_parent(a integer CONSTRAINT positive CHECK(a>0))",
    );
    for (table, expression) in [
        ("check_column_cast", "a::integer>0"),
        ("check_constant_cast", "a>'0'::integer"),
    ] {
        exec(&engine, &format!("CREATE TABLE {table}(a integer CONSTRAINT positive CHECK({expression})) INHERITS(check_parent)"));
        state(&engine, table, "positive", true, 1, true);
    }
    error(&engine, "CREATE TABLE check_duplicate(a integer CONSTRAINT positive CHECK(a>0),CONSTRAINT positive CHECK(a>0)) INHERITS(check_parent)", "42710");
}
