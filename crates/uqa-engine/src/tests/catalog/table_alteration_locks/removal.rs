//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inherited constraint removal follows origin metadata and retained child identities.

use super::{after_wait, error, peer_lock, sessions, sql, Engine, Value};

fn constraints(engine: &Engine, table: &str) -> Vec<(String, bool, i64)> {
    sql(engine, &format!("SELECT conname, conislocal, coninhcount FROM pg_constraint WHERE conrelid='{table}'::regclass AND contype IN ('n','c') ORDER BY conname"))
        .rows.into_iter().map(|row| {
            let Value::Str(name) = &row["conname"] else { panic!("constraint name") };
            let Value::Bool(local) = row["conislocal"] else { panic!("constraint origin") };
            let Value::Int(parents) = row["coninhcount"] else { panic!("constraint parents") };
            (name.to_string(), local, parents)
        }).collect()
}

#[test]
fn inherited_not_null_removal_reaches_descendants_and_restores_at_savepoints() {
    for provider in 0..3 {
        for action in ["DROP CONSTRAINT required", "ALTER COLUMN v DROP NOT NULL"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v; CREATE TABLE child() INHERITS(t); CREATE TABLE grandchild() INHERITS(child)");
            sql(
                &first,
                &format!("BEGIN; SAVEPOINT before_drop; ALTER TABLE t {action}"),
            );
            for table in ["t", "child", "grandchild"] {
                assert!(
                    constraints(&first, table).is_empty(),
                    "{provider}/{action}/{table}"
                );
                sql(&first, &format!("INSERT INTO {table} VALUES(NULL)"));
                peer_lock(&second, table, "ACCESS SHARE", false);
            }
            sql(&first, "ROLLBACK TO before_drop");
            for table in ["t", "child", "grandchild"] {
                assert_eq!(constraints(&first, table).len(), 1, "{provider}/{table}");
                peer_lock(&second, table, "ACCESS EXCLUSIVE", true);
            }
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn inherited_not_null_cannot_be_removed_directly() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT required NOT NULL v; CREATE TABLE child() INHERITS(t)",
        );
        for action in ["DROP CONSTRAINT required", "ALTER COLUMN v DROP NOT NULL"] {
            error(&first, &format!("ALTER TABLE child {action}"), "42P16");
            assert_eq!(
                constraints(&first, "child"),
                [("required".into(), false, 1)]
            );
        }
    }
}

#[test]
fn only_constraint_removal_makes_direct_children_local_without_locking_grandchildren() {
    for provider in 0..3 {
        for declaration in ["NOT NULL v", "CHECK(v>0)"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, &format!("ALTER TABLE t ADD CONSTRAINT required {declaration}; CREATE TABLE child() INHERITS(t); CREATE TABLE grandchild() INHERITS(child)"));
            sql(&first, "BEGIN; ALTER TABLE ONLY t DROP CONSTRAINT required");
            assert!(constraints(&first, "t").is_empty());
            assert_eq!(constraints(&first, "child"), [("required".into(), true, 0)]);
            assert_eq!(
                constraints(&first, "grandchild"),
                [("required".into(), false, 1)]
            );
            peer_lock(&second, "child", "ACCESS SHARE", false);
            peer_lock(&second, "grandchild", "ACCESS EXCLUSIVE", true);
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn constraint_removal_stops_at_local_or_other_parent_origins() {
    for provider in 0..3 {
        for local in [false, true] {
            let (_directory, first, second) = sessions(provider);
            if local {
                sql(&first, "CREATE TABLE child() INHERITS(t); CREATE TABLE grandchild() INHERITS(child); ALTER TABLE child ADD CONSTRAINT child_nn NOT NULL v; ALTER TABLE t ADD CONSTRAINT required NOT NULL v");
            } else {
                sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v; CREATE TABLE other(v integer CONSTRAINT other_nn NOT NULL); CREATE TABLE child() INHERITS(t,other); CREATE TABLE grandchild() INHERITS(child)");
            }
            sql(&first, "BEGIN; ALTER TABLE t DROP CONSTRAINT required");
            let child = constraints(&first, "child");
            assert_eq!(child.len(), 1);
            assert_eq!((child[0].1, child[0].2), (local, i64::from(!local)));
            assert_eq!(constraints(&first, "grandchild").len(), 1);
            peer_lock(&second, "child", "ACCESS SHARE", false);
            peer_lock(&second, "grandchild", "ACCESS EXCLUSIVE", true);
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn recursive_constraint_removal_preserves_child_identity_across_rename_and_name_reuse() {
    for provider in 0..3 {
        for declaration in ["NOT NULL v", "CHECK(v>0)"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, &format!("ALTER TABLE t ADD CONSTRAINT required {declaration}; CREATE TABLE child() INHERITS(t)"));
            sql(&first, &format!("BEGIN; ALTER TABLE child RENAME TO original; CREATE TABLE child(v integer); ALTER TABLE child ADD CONSTRAINT required {declaration}"));
            let (second, result) = after_wait(
                &first,
                second,
                "BEGIN; ALTER TABLE t DROP CONSTRAINT required",
                "public.child",
                "COMMIT",
            );
            result.unwrap();
            assert!(constraints(&second, "t").is_empty());
            assert!(constraints(&second, "original").is_empty());
            assert_eq!(constraints(&second, "child").len(), 1);
            peer_lock(&first, "original", "ACCESS SHARE", false);
            peer_lock(&first, "child", "ACCESS EXCLUSIVE", true);
            sql(&second, "ROLLBACK");
        }
    }
}

#[test]
fn noninherited_constraint_removal_leaves_children_unlocked() {
    for provider in 0..3 {
        for declaration in ["NOT NULL v", "CHECK(v>0)"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, &format!("ALTER TABLE t ADD CONSTRAINT required {declaration} NO INHERIT; CREATE TABLE child() INHERITS(t)"));
            sql(&first, "BEGIN; ALTER TABLE t DROP CONSTRAINT required");
            peer_lock(&second, "child", "ACCESS EXCLUSIVE", true);
            assert!(constraints(&first, "t").is_empty());
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn not_null_removal_matches_different_names_after_another_parent_is_removed() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT required NOT NULL v; CREATE TABLE other(v integer CONSTRAINT other_nn NOT NULL); CREATE TABLE child() INHERITS(other,t); CREATE TABLE grandchild() INHERITS(child)");
        sql(&first, "ALTER TABLE other DROP CONSTRAINT other_nn");
        assert_eq!(
            constraints(&first, "child"),
            [("other_nn".into(), false, 1)]
        );
        sql(&first, "ALTER TABLE t DROP CONSTRAINT required");
        for table in ["t", "child", "grandchild"] {
            assert!(constraints(&first, table).is_empty(), "{provider}/{table}");
            sql(&first, &format!("INSERT INTO {table} VALUES(NULL)"));
        }
    }
}

#[test]
fn diamond_constraint_removal_revisits_children_after_their_last_origin_disappears() {
    for provider in 0..3 {
        for declaration in ["NOT NULL v", "CHECK(v>0)"] {
            let (_directory, first, _second) = sessions(provider);
            sql(&first, &format!("ALTER TABLE t ADD CONSTRAINT required {declaration}; CREATE TABLE left_child() INHERITS(t); CREATE TABLE right_child() INHERITS(t); CREATE TABLE leaf() INHERITS(left_child,right_child)"));
            sql(&first, "ALTER TABLE t DROP CONSTRAINT required");
            for table in ["t", "left_child", "right_child", "leaf"] {
                assert!(
                    constraints(&first, table).is_empty(),
                    "{provider}/{declaration}/{table}"
                );
            }
        }
    }
}

#[test]
fn not_null_removal_preserves_identity_protection_and_serial_columns() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE TABLE identities(v integer GENERATED ALWAYS AS IDENTITY CONSTRAINT required NOT NULL); CREATE TABLE serials(v serial CONSTRAINT required NOT NULL)");
        error(
            &first,
            "ALTER TABLE identities DROP CONSTRAINT required",
            "55000",
        );
        error(
            &first,
            "ALTER TABLE identities ALTER COLUMN v DROP NOT NULL",
            "42601",
        );
        assert_eq!(
            constraints(&first, "identities"),
            [("required".into(), true, 0)]
        );
        sql(
            &first,
            "ALTER TABLE serials DROP CONSTRAINT required; INSERT INTO serials VALUES(NULL)",
        );
        assert!(constraints(&first, "serials").is_empty());
    }
}
