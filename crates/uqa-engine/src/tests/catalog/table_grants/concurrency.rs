//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary relation ACLs coordinate by catalog tuple, including independently writable attributes.

use crate::tests::relation_lock_support::{after_tuple_wait, sessions, sql};
use uqa_core::Value;

#[test]
fn ordinary_relation_acl_changes_wait_for_the_original_catalog_tuple() {
    for provider in 0..3 {
        for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO acl; COMMIT"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE ROLE other");
            let row = sql(&first, "SELECT 't'::regclass::oid AS oid");
            let Value::Int(oid) = row.rows[0]["oid"] else {
                panic!("relation OID missing");
            };
            sql(&first, "BEGIN; SAVEPOINT acl; GRANT SELECT ON t TO reader");
            let (_, result) = after_tuple_wait(
                &first,
                second,
                "GRANT UPDATE ON t TO other",
                "pg_catalog.pg_class",
                u64::try_from(oid).unwrap(),
                finish,
            );
            if finish == "COMMIT" {
                assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
            } else {
                result.unwrap();
            }
        }
    }
}

#[test]
fn independent_ordinary_attribute_acls_commit_without_replacing_each_other() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE reader; CREATE TABLE acl_columns(a integer, b integer)",
        );
        sql(&first, "BEGIN; GRANT SELECT(a) ON acl_columns TO reader");
        sql(&second, "GRANT SELECT(b) ON acl_columns TO reader");
        sql(&first, "COMMIT");
        for column in ["a", "b"] {
            let result = sql(&second, &format!("SELECT has_column_privilege('reader', 'acl_columns', '{column}', 'SELECT') AS allowed"));
            assert_eq!(
                result.rows[0]["allowed"],
                Value::Bool(true),
                "provider {provider}, column {column}"
            );
        }
    }
}

#[test]
fn private_ordinary_attribute_acls_merge_fresh_peer_authority() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE reader; CREATE TABLE acl_columns(a integer, b integer)",
        );
        sql(
            &first,
            "BEGIN ISOLATION LEVEL REPEATABLE READ; GRANT SELECT(a) ON acl_columns TO reader",
        );
        sql(&second, "GRANT SELECT(b) ON acl_columns TO reader");
        for column in ["a", "b"] {
            let result = sql(&first, &format!("SELECT has_column_privilege('reader', 'acl_columns', '{column}', 'SELECT') AS allowed"));
            assert_eq!(
                result.rows[0]["allowed"],
                Value::Bool(true),
                "provider {provider}, column {column}"
            );
        }
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn table_shaped_attribute_acls_keep_private_and_peer_changes_at_every_isolation() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE TABLE acl_columns(a int, b int); CREATE VIEW acl_view AS SELECT * FROM acl_columns; CREATE MATERIALIZED VIEW acl_mat AS SELECT * FROM acl_columns; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE acl_foreign(a int, b int) SERVER remote");
            let relations = ["acl_columns", "acl_view", "acl_mat", "acl_foreign"];
            sql(
                &first,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT acl"),
            );
            for relation in relations {
                sql(&first, &format!("GRANT SELECT(a) ON {relation} TO reader"));
            }
            for relation in relations {
                sql(&second, &format!("GRANT SELECT(b) ON {relation} TO reader"));
            }
            for relation in relations {
                for column in ["a", "b"] {
                    let result = sql(&first, &format!("SELECT has_column_privilege('reader', '{relation}', '{column}', 'SELECT') AS allowed"));
                    assert_eq!(
                        result.rows[0]["allowed"],
                        Value::Bool(true),
                        "{provider}/{isolation}/{relation}/{column}"
                    );
                }
            }
            sql(&first, "COMMIT");
            let fresh = second.new_session().unwrap();
            for relation in relations {
                for column in ["a", "b"] {
                    let result = sql(&fresh, &format!("SELECT has_column_privilege('reader', '{relation}', '{column}', 'SELECT') AS allowed"));
                    assert_eq!(
                        result.rows[0]["allowed"],
                        Value::Bool(true),
                        "committed {provider}/{isolation}/{relation}/{column}"
                    );
                }
            }
        }
    }
}

#[test]
fn equal_acl_updates_and_grant_revoke_cycles_replace_the_catalog_tuple() {
    for provider in 0..3 {
        for (initial, holder, waiter, attribute) in [
            (
                "GRANT UPDATE(v) ON t TO reader",
                "REVOKE SELECT ON t FROM reader",
                "GRANT SELECT(v) ON t TO other",
                true,
            ),
            (
                "GRANT UPDATE(v) ON t TO reader",
                "REVOKE SELECT(v) ON t FROM reader",
                "GRANT SELECT(v) ON t TO other",
                true,
            ),
            (
                "GRANT SELECT ON t TO reader",
                "GRANT SELECT ON t TO reader",
                "GRANT UPDATE ON t TO other",
                false,
            ),
            (
                "GRANT SELECT(v) ON t TO reader",
                "GRANT SELECT(v) ON t TO reader",
                "GRANT UPDATE(v) ON t TO other",
                true,
            ),
            (
                "",
                "GRANT SELECT ON t TO reader; REVOKE SELECT ON t FROM reader",
                "GRANT UPDATE ON t TO other",
                false,
            ),
            (
                "",
                "GRANT SELECT(v) ON t TO reader; REVOKE SELECT(v) ON t FROM reader",
                "GRANT UPDATE(v) ON t TO other",
                true,
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE ROLE other");
            if !initial.is_empty() {
                sql(&first, initial);
            }
            let Value::Int(oid) = sql(&first, "SELECT 't'::regclass::oid AS oid").rows[0]["oid"]
            else {
                panic!("missing OID");
            };
            sql(&first, &format!("BEGIN; {holder}"));
            let oid = u64::try_from(oid).unwrap();
            let (catalog, key) = if attribute {
                ("pg_catalog.pg_attribute", (oid << 16) | 1)
            } else {
                ("pg_catalog.pg_class", oid)
            };
            let (_, result) = after_tuple_wait(&first, second, waiter, catalog, key, "COMMIT");
            assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
        }
    }
}

#[test]
fn unrelated_acl_tuples_and_empty_attribute_updates_do_not_hold_each_other() {
    use std::{sync::mpsc, thread, time::Duration};
    for provider in 0..3 {
        for (initial, holder, waiter) in [
            (
                "",
                "GRANT SELECT ON t TO reader",
                "GRANT UPDATE(v) ON t TO other",
            ),
            (
                "",
                "GRANT SELECT(v) ON t TO reader",
                "GRANT UPDATE ON t TO other",
            ),
            (
                "",
                "REVOKE SELECT(v) ON t FROM reader",
                "GRANT SELECT(v) ON t TO other",
            ),
            (
                "GRANT SELECT ON t TO reader; SET ROLE reader",
                "GRANT SELECT(v) ON t TO other",
                "GRANT SELECT(v) ON t TO other",
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE ROLE other");
            if !initial.is_empty() {
                sql(&first, initial);
            }
            sql(&first, &format!("BEGIN; {holder}"));
            let (send, done) = mpsc::channel();
            let worker = thread::spawn(move || {
                let result = second.sql(waiter, &[]);
                send.send(result).unwrap();
            });
            let result = done.recv_timeout(Duration::from_secs(30));
            sql(&first, "COMMIT");
            worker.join().unwrap();
            result
                .unwrap_or_else(|error| {
                    panic!("unexpected ACL wait for {holder} / {waiter}: {error}")
                })
                .unwrap();
        }
    }
}

#[test]
fn attribute_wait_refresh_only_visits_columns_that_follow_the_waited_tuple() {
    use crate::tests::relation_lock_support::after_tuple_wait_with_release;
    for provider in 0..3 {
        for waiting_column in ["a", "b"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE ROLE other; CREATE TABLE acl_columns(a int, b int); CREATE VIEW acl_view AS SELECT * FROM acl_columns; CREATE MATERIALIZED VIEW acl_mat AS SELECT * FROM acl_columns; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE acl_foreign(a int, b int) SERVER remote");
            let mut second = second;
            for relation in [
                "acl_columns",
                "acl_view",
                "acl_mat",
                "acl_foreign",
                "pg_catalog.pg_class",
            ] {
                let (column, peer_column, attribute) = match (relation, waiting_column) {
                    ("pg_catalog.pg_class", "a") => ("relname", "relnamespace", 2),
                    ("pg_catalog.pg_class", _) => ("relnamespace", "relname", 3),
                    (_, "a") => ("a", "b", 1),
                    _ => ("b", "a", 2),
                };
                // System relations initially grant SELECT to PUBLIC; use UPDATE to isolate attribute grants.
                sql(
                    &first,
                    &format!("GRANT UPDATE({column}) ON {relation} TO reader"),
                );
                let Value::Int(oid) = sql(
                    &first,
                    &format!("SELECT '{relation}'::regclass::oid AS oid"),
                )
                .rows[0]["oid"] else {
                    panic!("missing relation OID")
                };
                sql(
                    &first,
                    &format!("BEGIN; GRANT UPDATE({column}) ON {relation} TO other"),
                );
                let peer = first.new_session().unwrap();
                let (worker, result) = after_tuple_wait_with_release(
                    &first,
                    second,
                    &format!("REVOKE UPDATE ON {relation} FROM reader"),
                    "pg_catalog.pg_attribute",
                    (u64::try_from(oid).unwrap() << 16) | attribute,
                    || {
                        sql(
                            &peer,
                            &format!("GRANT UPDATE({peer_column}) ON {relation} TO reader"),
                        );
                        first.sql("ROLLBACK", &[])
                    },
                );
                second = worker;
                result.unwrap();
                for (name, expected) in [(column, false), (peer_column, waiting_column == "b")] {
                    let row = sql(&second, &format!("SELECT has_column_privilege('reader', '{relation}', '{name}', 'UPDATE') AS allowed"));
                    assert_eq!(
                        row.rows[0]["allowed"],
                        Value::Bool(expected),
                        "{provider}/{relation}/{column}/{name}"
                    );
                }
            }
        }
    }
}
