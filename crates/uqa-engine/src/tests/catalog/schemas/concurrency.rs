//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observed namespace tuple and lifetime waits against `PostgreSQL` command outcomes.

use crate::tests::relation_lock_support::{after_shared_wait, after_tuple_wait, sessions, sql};
use uqa_core::Value;
use uqa_execution::{
    row_locks::shared_objects::SharedCatalogLock,
    schema::namespaces::identity::SCHEMA_CATALOG_CLASS_ID,
};

#[test]
fn schema_acl_waits_follow_commit_abort_and_savepoint_undo_at_every_isolation() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_acl; COMMIT"] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE reader; CREATE SCHEMA s");
                sql(
                    &first,
                    "BEGIN; SAVEPOINT before_acl; GRANT USAGE ON SCHEMA s TO reader",
                );
                let oid = first.durable.schemas.read()["s"].tuple.unwrap().oid as u64;
                sql(
                    &second,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
                );
                let (second, result) = after_tuple_wait(
                    &first,
                    second,
                    "GRANT CREATE ON SCHEMA s TO reader",
                    "pg_catalog.pg_namespace",
                    oid,
                    finish,
                );
                if finish == "COMMIT" {
                    let error = result.unwrap_err();
                    assert_eq!(
                        error.sqlstate(),
                        Some("XX000"),
                        "{provider}/{isolation}: {error}"
                    );
                    assert!(error.to_string().contains("tuple concurrently updated"));
                    sql(&second, "ROLLBACK");
                } else {
                    result.unwrap();
                    sql(&second, "COMMIT");
                }
                let row = &sql(&first, "SELECT has_schema_privilege('reader', 's', 'USAGE') AS usage, has_schema_privilege('reader', 's', 'CREATE') AS create").rows[0];
                assert_eq!(row["usage"], Value::Bool(finish == "COMMIT"));
                assert_eq!(row["create"], Value::Bool(finish != "COMMIT"));
            }
        }
    }
}

#[test]
fn namespace_owner_and_drop_keep_the_original_tuple_through_concurrent_changes() {
    for provider in 0..3 {
        for (holder, waiter, action) in [
            (
                "GRANT USAGE ON SCHEMA s TO reader",
                "ALTER SCHEMA s OWNER TO owner",
                "updated",
            ),
            (
                "ALTER SCHEMA s OWNER TO owner",
                "GRANT USAGE ON SCHEMA s TO reader",
                "updated",
            ),
            ("ALTER SCHEMA s OWNER TO owner", "DROP SCHEMA s", "updated"),
            ("DROP SCHEMA s", "ALTER SCHEMA s OWNER TO owner", "deleted"),
            (
                "DROP SCHEMA s; CREATE SCHEMA s",
                "ALTER SCHEMA s OWNER TO owner",
                "deleted",
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE ROLE reader; CREATE ROLE owner; CREATE SCHEMA s",
            );
            let oid = first.durable.schemas.read()["s"].tuple.unwrap().oid as u64;
            sql(&first, &format!("BEGIN; {holder}"));
            let (_, result) = after_tuple_wait(
                &first,
                second,
                waiter,
                "pg_catalog.pg_namespace",
                oid,
                "COMMIT",
            );
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some("XX000"),
                "{provider}/{holder}/{waiter}: {error}"
            );
            assert!(
                error
                    .to_string()
                    .contains(&format!("tuple concurrently {action}")),
                "{error}"
            );
        }
    }
}

#[test]
fn namespace_lifetime_waits_rebind_grants_and_delete_fresh_acl_tuples() {
    for provider in 0..3 {
        for (holder, waiter, missing) in [
            ("GRANT USAGE ON SCHEMA s TO reader", "DROP SCHEMA s", false),
            ("DROP SCHEMA s", "GRANT USAGE ON SCHEMA s TO reader", true),
            (
                "DROP SCHEMA s; CREATE SCHEMA s",
                "GRANT USAGE ON SCHEMA s TO reader",
                false,
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE SCHEMA s");
            let oid = first.durable.schemas.read()["s"].tuple.unwrap().oid as u32;
            sql(&first, &format!("BEGIN; {holder}"));
            let (_, result) = after_shared_wait(
                &first,
                second,
                waiter,
                SharedCatalogLock::Object {
                    class_id: SCHEMA_CATALOG_CLASS_ID,
                    oid,
                },
                "COMMIT",
            );
            if missing {
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some("3F000"), "{error}");
            } else {
                result.unwrap();
                if waiter.starts_with("GRANT") {
                    assert_eq!(
                        sql(
                            &first,
                            "SELECT has_schema_privilege('reader', 's', 'USAGE') AS allowed"
                        )
                        .rows[0]["allowed"],
                        Value::Bool(true)
                    );
                } else {
                    assert!(!first.has_schema("s").unwrap());
                }
            }
        }
    }
}

#[test]
fn unchanged_and_warned_schema_acl_commands_still_replace_catalog_tuples() {
    for provider in 0..3 {
        for (setup, holder) in [
            (
                "GRANT USAGE ON SCHEMA s TO reader",
                "GRANT USAGE ON SCHEMA s TO reader",
            ),
            ("", "REVOKE USAGE ON SCHEMA s FROM reader"),
            (
                "GRANT USAGE ON SCHEMA s TO reader",
                "SET ROLE reader; GRANT USAGE ON SCHEMA s TO owner",
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE ROLE reader; CREATE ROLE owner; CREATE SCHEMA s",
            );
            if !setup.is_empty() {
                sql(&first, setup);
            }
            let before = first.durable.schemas.read()["s"].clone();
            sql(&first, &format!("BEGIN; {holder}"));
            let after = first.durable.schemas.read()["s"].clone();
            assert_ne!(
                before.tuple.unwrap().revision,
                after.tuple.unwrap().revision
            );
            let (_, result) = after_tuple_wait(
                &first,
                second,
                "GRANT CREATE ON SCHEMA s TO owner",
                "pg_catalog.pg_namespace",
                before.tuple.unwrap().oid as u64,
                "COMMIT",
            );
            assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
        }
    }
}

#[test]
fn same_name_schema_creation_reserves_the_name_until_commit_or_undo() {
    for provider in 0..3 {
        for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_schema; COMMIT"] {
            for clause in ["", "IF NOT EXISTS "] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "BEGIN; SAVEPOINT before_schema; CREATE SCHEMA s");
                let first_id = first.durable.schemas.read()["s"].tuple.unwrap();
                let (_, result) = after_shared_wait(
                    &first,
                    second,
                    &format!("CREATE SCHEMA {clause}s"),
                    SharedCatalogLock::Name {
                        class_id: SCHEMA_CATALOG_CLASS_ID,
                        name: "s",
                    },
                    finish,
                );
                if finish == "COMMIT" {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("23505"));
                } else {
                    result.unwrap();
                    assert!(first.has_schema("s").unwrap());
                    assert_ne!(
                        first.durable.schemas.read()["s"].tuple.unwrap().object_id,
                        first_id.object_id
                    );
                }
            }
        }
    }
}

#[test]
fn restrict_validation_and_equal_owner_assignment_do_not_wait_on_a_schema_tuple() {
    for provider in 0..3 {
        for occupied in [true, false] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE ROLE reader; CREATE ROLE owner; CREATE SCHEMA s",
            );
            if occupied {
                sql(&first, "CREATE TABLE s.child(id integer)");
                sql(&second, "SELECT * FROM s.child");
                sql(&first, "BEGIN; ALTER SCHEMA s OWNER TO owner");
            } else {
                let before = first.durable.schemas.read()["s"].tuple;
                sql(&first, "BEGIN; ALTER SCHEMA s OWNER TO uqa");
                assert_eq!(first.durable.schemas.read()["s"].tuple, before);
            }
            let cancellation = second.runtime.cancellation.clone();
            let (send, done) = std::sync::mpsc::channel();
            let task = std::thread::spawn(move || {
                let result = second.sql(
                    if occupied {
                        "DROP SCHEMA s"
                    } else {
                        "GRANT USAGE ON SCHEMA s TO reader"
                    },
                    &[],
                );
                let _ = send.send(result);
            });
            let result = done.recv_timeout(std::time::Duration::from_secs(30));
            if result.is_err() {
                cancellation.cancel();
            }
            sql(&first, "COMMIT");
            task.join().unwrap();
            let result = result.unwrap_or_else(|error| {
                panic!(
                    "{provider}/{occupied}: command must finish before the holder commits: {error}"
                )
            });
            if occupied {
                assert_eq!(result.unwrap_err().sqlstate(), Some("2BP01"));
            } else {
                result.unwrap();
            }
        }
    }
}

#[test]
fn namespace_mutations_honor_explicit_catalog_relation_locks() {
    for provider in 0..3 {
        for statement in [
            "CREATE SCHEMA created",
            "GRANT USAGE ON SCHEMA s TO reader",
            "ALTER SCHEMA s OWNER TO owner",
            "DROP SCHEMA s",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE ROLE reader; CREATE ROLE owner; CREATE SCHEMA s",
            );
            sql(
                &first,
                "BEGIN; LOCK TABLE pg_catalog.pg_namespace IN ACCESS EXCLUSIVE MODE",
            );
            let (_, result) = crate::tests::relation_lock_support::after_wait(
                &first,
                second,
                statement,
                "pg_catalog.pg_namespace",
                "COMMIT",
            );
            result.unwrap();
        }
    }
}

#[test]
fn a_later_schema_tuple_conflict_rolls_back_earlier_grant_targets() {
    for provider in 0..3 {
        for finish in ["COMMIT", "ROLLBACK"] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE ROLE reader; CREATE SCHEMA a; CREATE SCHEMA b",
            );
            sql(&first, "BEGIN; GRANT USAGE ON SCHEMA b TO reader");
            let oid = first.durable.schemas.read()["b"].tuple.unwrap().oid as u64;
            let (_, result) = after_tuple_wait(
                &first,
                second,
                "GRANT CREATE ON SCHEMA a, b TO reader",
                "pg_catalog.pg_namespace",
                oid,
                finish,
            );
            if finish == "COMMIT" {
                assert_eq!(result.unwrap_err().sqlstate(), Some("XX000"));
            } else {
                result.unwrap();
            }
            let result = sql(&first, "SELECT has_schema_privilege('reader', 'a', 'CREATE') AS a, has_schema_privilege('reader', 'b', 'CREATE') AS b");
            for name in ["a", "b"] {
                assert_eq!(result.rows[0][name], Value::Bool(finish != "COMMIT"));
            }
        }
    }
}
