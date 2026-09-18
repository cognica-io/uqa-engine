//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared role dependency waits preserve original identity and private transaction state.

use super::{coordination::after_wait, identity::reopen};
use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use uqa_core::Value;
use uqa_execution::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};

struct Target {
    setup: &'static str,
    grant: &'static str,
    inquiry: &'static str,
}

const TARGETS: &[Target] = &[
    Target { setup: "", grant: "GRANT SELECT ON t TO dependent", inquiry: "has_table_privilege('dependent', 't', 'SELECT')" },
    Target { setup: "", grant: "GRANT SELECT(v) ON t TO dependent", inquiry: "has_column_privilege('dependent', 't', 'v', 'SELECT')" },
    Target { setup: "", grant: "GRANT UPDATE ON pg_class TO dependent WITH GRANT OPTION", inquiry: "has_table_privilege('dependent', 'pg_class', 'UPDATE WITH GRANT OPTION')" },
    Target { setup: "", grant: "GRANT UPDATE(relname) ON pg_class TO dependent", inquiry: "has_column_privilege('dependent', 'pg_class', 'relname', 'UPDATE')" },
    Target { setup: "CREATE VIEW role_view AS SELECT v FROM t", grant: "GRANT SELECT ON role_view TO dependent", inquiry: "has_table_privilege('dependent', 'role_view', 'SELECT')" },
    Target { setup: "CREATE MATERIALIZED VIEW role_view AS SELECT v FROM t", grant: "GRANT SELECT ON role_view TO dependent", inquiry: "has_table_privilege('dependent', 'role_view', 'SELECT')" },
    Target { setup: "CREATE SERVER role_remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE role_foreign(v integer) SERVER role_remote", grant: "GRANT SELECT ON role_foreign TO dependent", inquiry: "has_table_privilege('dependent', 'role_foreign', 'SELECT')" },
];

fn role_lock(engine: &Engine) -> SharedCatalogLock<'static> {
    SharedCatalogLock::Object {
        class_id: ROLE_CATALOG_CLASS_ID,
        oid: u32::try_from(engine.durable.roles.read()["dependent"].oid).unwrap(),
    }
}

fn role_exists(engine: &Engine) -> bool {
    sql(
        engine,
        "SELECT count(*) AS n FROM pg_roles WHERE rolname = 'dependent'",
    )
    .rows[0]["n"]
        == Value::Int(1)
}

fn allowed(engine: &Engine, target: &Target) -> bool {
    sql(engine, &format!("SELECT {} AS allowed", target.inquiry)).rows[0]["allowed"]
        == Value::Bool(true)
}

#[test]
fn role_drop_waits_for_added_relation_and_column_acl_dependencies() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for target in TARGETS {
                for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_grant; COMMIT"] {
                    let (directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE dependent");
                    if !target.setup.is_empty() {
                        sql(&first, target.setup);
                    }
                    let lock = role_lock(&first);
                    sql(&first, "BEGIN; SAVEPOINT before_grant");
                    sql(&first, target.grant);
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                    );
                    let (second, result) =
                        after_wait(&first, second, "DROP ROLE dependent", lock, finish);
                    if finish == "COMMIT" {
                        assert_eq!(result.unwrap_err().sqlstate(), Some("2BP01"));
                        sql(&second, "ROLLBACK");
                        assert!(
                            allowed(&first, target),
                            "provider {provider}, {isolation}, {}, {finish}",
                            target.grant
                        );
                    } else {
                        result.unwrap();
                        sql(&second, "COMMIT");
                    }
                    assert_eq!(role_exists(&first), finish == "COMMIT");
                    drop(second);
                    drop(first);
                    let restored = reopen(provider, &directory.path().join("table-locks.db"));
                    assert_eq!(role_exists(&restored), finish == "COMMIT");
                }
            }
        }
    }
}

#[test]
fn acl_grants_wait_for_role_drop_and_never_rebind_a_recreated_name() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for target in TARGETS {
                for finish in [
                    "COMMIT",
                    "ROLLBACK",
                    "ROLLBACK TO before_drop; COMMIT",
                    "RECREATE",
                ] {
                    let (directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE dependent");
                    if !target.setup.is_empty() {
                        sql(&first, target.setup);
                    }
                    let lock = role_lock(&first);
                    let original = first.durable.roles.read()["dependent"].object_id;
                    sql(&first, "BEGIN; SAVEPOINT before_drop; DROP ROLE dependent");
                    if finish == "RECREATE" {
                        sql(&first, "CREATE ROLE dependent");
                        assert_ne!(first.durable.roles.read()["dependent"].object_id, original);
                    }
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2)"),
                    );
                    let (second, result) = after_wait(
                        &first,
                        second,
                        target.grant,
                        lock,
                        if finish == "RECREATE" {
                            "COMMIT"
                        } else {
                            finish
                        },
                    );
                    let granted = finish != "COMMIT" && finish != "RECREATE";
                    if granted {
                        result.unwrap();
                        sql(&second, "COMMIT");
                        assert!(
                            allowed(&first, target),
                            "provider {provider}, {isolation}, {}, {finish}",
                            target.grant
                        );
                    } else {
                        assert_eq!(result.unwrap_err().sqlstate(), Some("42704"));
                        sql(&second, "ROLLBACK");
                        if finish == "RECREATE" {
                            assert!(!allowed(&first, target));
                        }
                    }
                    assert_eq!(
                        sql(&first, "SELECT v FROM t").rows.len(),
                        if granted { 2 } else { 1 }
                    );
                    drop(second);
                    drop(first);
                    let restored = reopen(provider, &directory.path().join("table-locks.db"));
                    assert_eq!(role_exists(&restored), finish != "COMMIT");
                    if finish != "COMMIT" {
                        assert_eq!(allowed(&restored, target), granted);
                    }
                }
            }
        }
    }
}

#[test]
fn unchanged_acl_roles_and_revoke_do_not_acquire_new_dependency_locks() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE dependent; GRANT SELECT ON t TO dependent",
        );
        let key = first.row_locks.shared_catalog_key(role_lock(&first));
        for statement in [
            "GRANT SELECT ON t TO dependent",
            "REVOKE SELECT ON t FROM dependent",
        ] {
            sql(&first, "BEGIN");
            sql(&first, statement);
            assert!(first
                .row_locks
                .try_acquire_relation(
                    second.session_id,
                    key,
                    RelationLockMode::AccessExclusive,
                    0,
                    &second.runtime.cancellation
                )
                .unwrap());
            first.row_locks.release_session(second.session_id);
            sql(&first, "ROLLBACK");
        }
    }
}
