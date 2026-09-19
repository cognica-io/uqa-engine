//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit index removal refreshes identity, table binding and authority before publication.

use crate::tests::relation_lock_support::{after_wait, sessions, sql};
use crate::Engine;
use uqa_core::catalog_identity::CatalogObjectIdentity;

fn index_identity(engine: &Engine) -> CatalogObjectIdentity {
    crate::catalog_indexes::index_definition(&engine.catalog_index("idx").unwrap().unwrap())
        .unwrap()
        .catalog
        .unwrap()
        .identity
}

fn peer_lock(engine: &Engine, table: &str, mode: &str, expected: bool) {
    sql(engine, "BEGIN");
    let result = engine.sql(&format!("LOCK TABLE {table} IN {mode} MODE NOWAIT"), &[]);
    sql(engine, "ROLLBACK");
    if expected {
        result.unwrap();
    } else {
        assert_eq!(result.unwrap_err().sqlstate(), Some("55P03"));
    }
}

#[test]
fn dropped_index_rebinds_its_table_and_physical_method_and_preserves_peer_changes_on_undo() {
    for provider in 0..3 {
        for rename_table in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE TABLE other(v text); CREATE INDEX idx ON t(v)",
            );
            let original = index_identity(&first);
            let (change, target) = if rename_table {
                (
                    "BEGIN; ALTER TABLE t RENAME TO moved; CREATE TABLE t(v integer)",
                    "moved",
                )
            } else {
                (
                    "BEGIN; DROP INDEX idx; CREATE INDEX idx ON other USING gin(v)",
                    "other",
                )
            };
            sql(&first, change);
            let (second, result) = after_wait(
                &first,
                second,
                "BEGIN; SAVEPOINT before_drop; DROP INDEX idx",
                "public.t",
                "COMMIT",
            );
            result.unwrap();
            assert!(second.catalog_index("idx").unwrap().is_none());
            let current = index_identity(&first);
            assert_eq!(original == current, rename_table);
            peer_lock(&first, "t", "ACCESS EXCLUSIVE", true);
            peer_lock(&first, target, "ACCESS SHARE", false);
            if !rename_table {
                assert!(second.fts_fields_for_table("other").unwrap().is_empty());
            }
            sql(&second, "ROLLBACK TO before_drop");
            let restored = sql(&second, "SELECT oid FROM pg_class WHERE relname = 'idx'");
            assert_eq!(restored.rows.len(), 1);
            assert_eq!(restored.rows[0]["oid"], uqa_core::Value::Int(current.oid));
            assert_eq!(
                index_identity(&second),
                current,
                "provider {provider}, rename table {rename_table}"
            );
            if !rename_table {
                assert_eq!(second.fts_fields_for_table("other").unwrap(), ["v"]);
            }
            peer_lock(&first, target, "ACCESS EXCLUSIVE", true);
            sql(&second, "ROLLBACK");
            assert_eq!(index_identity(&second), current);
        }
    }
}

#[test]
fn index_drop_rechecks_disappearance_wrong_kind_and_constraint_ownership_after_wait() {
    for provider in 0..3 {
        for (change, statement, expected) in [
            (
                "DROP INDEX idx; CREATE TABLE idx(v integer)",
                "DROP INDEX IF EXISTS idx",
                Some("42809"),
            ),
            (
                "DROP INDEX idx; ALTER TABLE other ADD CONSTRAINT idx UNIQUE(v)",
                "DROP INDEX idx",
                Some("2BP01"),
            ),
            ("DROP INDEX idx", "DROP INDEX idx", Some("42704")),
            ("DROP INDEX idx", "DROP INDEX IF EXISTS idx", None),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE TABLE other(v integer); CREATE INDEX idx ON t(v)",
            );
            sql(&first, &format!("BEGIN; {change}"));
            let (second, result) = after_wait(
                &first,
                second,
                &format!("BEGIN; {statement}"),
                "public.t",
                "COMMIT",
            );
            if let Some(expected) = expected {
                assert_eq!(result.unwrap_err().sqlstate(), Some(expected), "{change}");
            } else {
                result.unwrap();
                peer_lock(&first, "t", "ACCESS EXCLUSIVE", true);
            }
            sql(&second, "ROLLBACK");
            if expected == Some("2BP01") {
                assert!(first.catalog_index("idx").unwrap().is_some());
            } else if expected == Some("42809") {
                sql(&first, "INSERT INTO idx VALUES(5)");
            } else {
                assert!(first.catalog_index("idx").unwrap().is_none());
            }
        }
    }
}

#[test]
fn constraint_index_drop_waits_for_the_table_before_rejecting_the_dependency() {
    for provider in 0..3 {
        for kind in ["PRIMARY KEY", "UNIQUE"] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                &format!("ALTER TABLE t ADD CONSTRAINT idx {kind}(v)"),
            );
            sql(&first, "BEGIN; LOCK TABLE t IN ACCESS EXCLUSIVE MODE");
            let (second, result) = after_wait(
                &first,
                second,
                "BEGIN; DROP INDEX idx",
                "public.t",
                "COMMIT",
            );
            assert_eq!(result.unwrap_err().sqlstate(), Some("2BP01"));
            sql(&second, "ROLLBACK");
            assert!(first.catalog_index("idx").unwrap().is_some());
        }
    }
}

#[test]
fn partitioned_index_removal_locks_descendant_tables_until_savepoint_undo() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(v integer) PARTITION BY RANGE(v); CREATE TABLE child PARTITION OF p FOR VALUES FROM(0) TO(10); CREATE INDEX idx ON p(v)");
        sql(&first, "BEGIN; LOCK TABLE child IN ACCESS EXCLUSIVE MODE");
        let (second, result) = after_wait(
            &first,
            second,
            "BEGIN; SAVEPOINT retained; DROP INDEX idx",
            "public.child",
            "COMMIT",
        );
        result.unwrap();
        peer_lock(&first, "p", "ACCESS SHARE", false);
        peer_lock(&first, "child", "ACCESS SHARE", false);
        sql(&second, "ROLLBACK TO retained");
        assert!(second.catalog_index("idx").unwrap().is_some());
        peer_lock(&first, "p", "ACCESS EXCLUSIVE", true);
        peer_lock(&first, "child", "ACCESS EXCLUSIVE", true);
        sql(&second, "ROLLBACK");

        sql(&first, "CREATE TABLE ordinary(v integer); CREATE TABLE inherited() INHERITS(ordinary); CREATE INDEX ordinary_idx ON ordinary(v)");
        sql(&second, "BEGIN; DROP INDEX ordinary_idx");
        peer_lock(&first, "ordinary", "ACCESS SHARE", false);
        peer_lock(&first, "inherited", "ACCESS EXCLUSIVE", true);
        sql(&second, "ROLLBACK");
    }
}

#[test]
fn index_drop_rechecks_current_owner_under_read_committed_and_repeatable_read() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE index_owner; GRANT USAGE ON SCHEMA public TO index_owner; CREATE INDEX idx ON t(v); ALTER TABLE t OWNER TO index_owner");
            sql(&second, "SET ROLE index_owner");
            sql(&first, "BEGIN; ALTER TABLE t OWNER TO uqa");
            let (second, result) = after_wait(
                &first,
                second,
                &format!("BEGIN ISOLATION LEVEL {isolation}; DROP INDEX idx"),
                "public.t",
                "COMMIT",
            );
            assert_eq!(result.unwrap_err().sqlstate(), Some("42501"));
            sql(&second, "ROLLBACK; RESET ROLE");
            assert!(first.catalog_index("idx").unwrap().is_some());
            peer_lock(&first, "t", "ACCESS EXCLUSIVE", true);
        }
    }
}
