//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owner dependency publication, rollback, and role incarnation binding across waits.

use super::{coordination::after_wait, identity::reopen};
use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use uqa_core::Value;
use uqa_execution::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::{
        shared_objects::{SharedCatalogLock, SharedObjectLockSession},
        RelationLockMode,
    },
};

mod quoted_names;
mod target_wait;

struct Target {
    setup: &'static str,
    object: &'static str,
    catalog: &'static str,
    owner_column: &'static str,
    condition: &'static str,
    relation: Option<&'static str>,
}

const TARGETS: &[Target] = &[
    Target { setup: "CREATE TABLE owner_table(v int)", object: "TABLE owner_table", catalog: "pg_class", owner_column: "relowner", condition: "relname = 'owner_table'", relation: Some("owner_table") },
    Target { setup: "CREATE TABLE owner_table(id serial)", object: "TABLE owner_table", catalog: "pg_class", owner_column: "relowner", condition: "relname IN ('owner_table', 'owner_table_id_seq')", relation: Some("owner_table") },
    Target { setup: "CREATE VIEW owner_view AS SELECT 1 AS v", object: "VIEW owner_view", catalog: "pg_class", owner_column: "relowner", condition: "relname = 'owner_view'", relation: Some("owner_view") },
    Target { setup: "CREATE MATERIALIZED VIEW owner_matview AS SELECT 1 AS v", object: "MATERIALIZED VIEW owner_matview", catalog: "pg_class", owner_column: "relowner", condition: "relname = 'owner_matview'", relation: Some("owner_matview") },
    Target { setup: "CREATE SERVER owner_remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE owner_foreign(v int) SERVER owner_remote", object: "FOREIGN TABLE owner_foreign", catalog: "pg_class", owner_column: "relowner", condition: "relname = 'owner_foreign'", relation: Some("owner_foreign") },
    Target { setup: "CREATE SEQUENCE owner_sequence", object: "SEQUENCE owner_sequence", catalog: "pg_class", owner_column: "relowner", condition: "relname = 'owner_sequence'", relation: Some("owner_sequence") },
    Target { setup: "CREATE SCHEMA owner_schema", object: "SCHEMA owner_schema", catalog: "pg_namespace", owner_column: "nspowner", condition: "nspname = 'owner_schema'", relation: None },
    Target { setup: "CREATE FUNCTION owner_function() RETURNS int LANGUAGE SQL AS 'SELECT 1'", object: "FUNCTION owner_function()", catalog: "pg_proc", owner_column: "proowner", condition: "proname = 'owner_function'", relation: None },
    Target { setup: "CREATE PROCEDURE owner_procedure() LANGUAGE SQL AS 'SELECT 1'", object: "PROCEDURE owner_procedure()", catalog: "pg_proc", owner_column: "proowner", condition: "proname = 'owner_procedure'", relation: None },
];

impl Target {
    fn alter(&self, owner: &str) -> String {
        format!("ALTER {} OWNER TO {owner}", self.object)
    }

    fn assert_owner(&self, engine: &Engine, expected: &str) {
        let query = format!(
            "SELECT r.rolname AS owner FROM {} c JOIN pg_roles r ON r.oid = c.{} WHERE {}",
            self.catalog, self.owner_column, self.condition
        );
        let result = sql(engine, &query);
        assert_eq!(
            result.rows.len(),
            if self.setup.contains("serial") { 2 } else { 1 }
        );
        for row in result.rows {
            assert_eq!(row["owner"], Value::Str(expected.into()), "{}", self.object);
        }
    }
}

fn role_lock(engine: &Engine) -> SharedCatalogLock<'static> {
    SharedCatalogLock::Object {
        class_id: ROLE_CATALOG_CLASS_ID,
        oid: u32::try_from(engine.durable.roles.read()["dependent"].oid).unwrap(),
    }
}

#[test]
fn owner_waits_keep_the_original_role_after_rename_and_name_reuse() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for target in TARGETS {
                let (directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE dependent");
                sql(&first, target.setup);
                let original = first.durable.roles.read()["dependent"].identity();
                let lock = role_lock(&first);
                sql(&first, "BEGIN");
                first
                    .acquire_shared_catalog(lock, RelationLockMode::AccessExclusive)
                    .unwrap()
                    .retain();
                sql(
                    &second,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                );
                let (second, result) = after_wait(
                    &first,
                    second,
                    &target.alter("dependent"),
                    lock,
                    "ALTER ROLE dependent RENAME TO renamed; CREATE ROLE dependent; COMMIT",
                );
                result.unwrap();
                sql(&second, "COMMIT");
                target.assert_owner(&first, "renamed");
                assert_eq!(first.durable.roles.read()["renamed"].identity(), original);
                drop(second);
                drop(first);
                target.assert_owner(
                    &reopen(provider, &directory.path().join("table-locks.db")),
                    "renamed",
                );
            }
        }
    }
}

#[test]
fn role_deletion_waits_for_owner_publication_and_observes_commit_or_undo() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for target in TARGETS {
                for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
                    let (directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE dependent");
                    sql(&first, target.setup);
                    let lock = role_lock(&first);
                    sql(&first, "BEGIN; INSERT INTO t VALUES (2); SAVEPOINT undo");
                    sql(&first, &target.alter("dependent"));
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                    );
                    let (second, result) =
                        after_wait(&first, second, "DROP ROLE dependent", lock, finish);
                    if finish == "COMMIT" {
                        assert_eq!(
                            result.unwrap_err().sqlstate(),
                            Some("2BP01"),
                            "{}",
                            target.object
                        );
                        sql(&second, "ROLLBACK");
                    } else {
                        result.unwrap();
                        sql(&second, "COMMIT");
                    }
                    let expected = if finish == "COMMIT" {
                        "dependent"
                    } else {
                        "uqa"
                    };
                    target.assert_owner(&first, expected);
                    assert_eq!(
                        sql(&first, "SELECT v FROM t").rows.len(),
                        if finish == "ROLLBACK" { 1 } else { 2 }
                    );
                    drop(second);
                    drop(first);
                    target.assert_owner(
                        &reopen(provider, &directory.path().join("table-locks.db")),
                        expected,
                    );
                }
            }
        }
    }
}

#[test]
fn owner_publication_waits_for_role_deletion_and_rejects_recreated_roles() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for target in TARGETS {
                for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT", "RECREATE"] {
                    let (directory, first, second) = sessions(provider);
                    sql(&first, "CREATE ROLE dependent");
                    sql(&first, target.setup);
                    let lock = role_lock(&first);
                    sql(&first, "BEGIN; SAVEPOINT undo; DROP ROLE dependent");
                    if finish == "RECREATE" {
                        sql(&first, "CREATE ROLE dependent");
                    }
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2)"),
                    );
                    let (second, result) = after_wait(
                        &first,
                        second,
                        &target.alter("dependent"),
                        lock,
                        if finish == "RECREATE" {
                            "COMMIT"
                        } else {
                            finish
                        },
                    );
                    let changed = !matches!(finish, "COMMIT" | "RECREATE");
                    if changed {
                        result.unwrap();
                        sql(&second, "COMMIT");
                    } else {
                        assert_eq!(
                            result.unwrap_err().sqlstate(),
                            Some("42704"),
                            "{}",
                            target.object
                        );
                        sql(&second, "ROLLBACK");
                    }
                    let expected = if changed { "dependent" } else { "uqa" };
                    target.assert_owner(&first, expected);
                    assert_eq!(
                        sql(&first, "SELECT v FROM t").rows.len(),
                        if changed { 2 } else { 1 }
                    );
                    drop(second);
                    drop(first);
                    target.assert_owner(
                        &reopen(provider, &directory.path().join("table-locks.db")),
                        expected,
                    );
                }
            }
        }
    }
}

#[test]
fn unchanged_owners_follow_object_permission_rules_without_new_role_dependency_locks() {
    for provider in 0..3 {
        for target in TARGETS {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE dependent; CREATE ROLE outsider");
            sql(&first, target.setup);
            sql(&first, &target.alter("dependent"));
            sql(&first, "SET ROLE outsider");
            if target.relation.is_some() {
                error(&first, &target.alter("dependent"), "42501");
            } else {
                sql(&first, &target.alter("dependent"));
            }
            sql(&first, "RESET ROLE; GRANT dependent TO outsider WITH INHERIT TRUE, SET FALSE; SET ROLE outsider");
            sql(&first, "BEGIN");
            sql(&first, &target.alter("dependent"));
            let key = first.row_locks.shared_catalog_key(role_lock(&first));
            assert!(
                first
                    .row_locks
                    .try_acquire_relation(
                        second.session_id,
                        key,
                        RelationLockMode::AccessExclusive,
                        0,
                        &second.runtime.cancellation
                    )
                    .unwrap(),
                "{}",
                target.object
            );
            second.row_locks.release_session(second.session_id);
            if target.setup.contains("serial") {
                sql(
                    &first,
                    "ALTER SEQUENCE owner_table_id_seq OWNER TO dependent",
                );
            }
            sql(&first, "ROLLBACK; RESET ROLE");
            target.assert_owner(&first, "dependent");
        }
    }
}

#[test]
fn routine_owner_transfer_requires_new_owner_namespace_create_except_for_no_op() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE ROLE original_owner; CREATE ROLE dependent; CREATE ROLE outsider; GRANT dependent TO original_owner; CREATE SCHEMA restricted; GRANT USAGE ON SCHEMA restricted TO original_owner, outsider; CREATE FUNCTION restricted.f() RETURNS int LANGUAGE SQL AS 'SELECT 1'; ALTER FUNCTION restricted.f() OWNER TO original_owner; CREATE PROCEDURE restricted.p() LANGUAGE SQL AS 'SELECT 1'; ALTER PROCEDURE restricted.p() OWNER TO original_owner");
        for object in ["FUNCTION restricted.f()", "PROCEDURE restricted.p()"] {
            sql(&first, "SET ROLE original_owner");
            error(
                &first,
                &format!("ALTER {object} OWNER TO dependent"),
                "42501",
            );
            sql(&first, "RESET ROLE; SET ROLE outsider");
            sql(&first, &format!("ALTER {object} OWNER TO original_owner"));
            sql(&first, "RESET ROLE; GRANT CREATE ON SCHEMA restricted TO dependent; SET ROLE original_owner");
            sql(&first, &format!("ALTER {object} OWNER TO dependent"));
            sql(
                &first,
                "RESET ROLE; REVOKE CREATE ON SCHEMA restricted FROM dependent",
            );
        }
    }
}

#[test]
fn implicit_system_schema_ownership_preserves_lookup_rollback_and_reopen() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE dependent; CREATE ROLE outsider");
        for schema in ["pg_catalog", "information_schema"] {
            sql(&first, &format!("SET ROLE outsider; ALTER SCHEMA {schema} OWNER TO uqa; RESET ROLE; ALTER SCHEMA {schema} OWNER TO dependent"));
            sql(&first, &format!("BEGIN; SAVEPOINT undo; ALTER SCHEMA {schema} OWNER TO uqa; ROLLBACK TO undo; COMMIT"));
            let result = sql(&second, &format!("SELECT r.rolname AS owner FROM pg_namespace n JOIN pg_roles r ON r.oid = n.nspowner WHERE n.nspname = '{schema}'"));
            assert_eq!(result.rows[0]["owner"], Value::Str("dependent".into()));
        }
        error(&first, "DROP ROLE dependent", "2BP01");
        drop(second);
        drop(first);
        let engine = reopen(provider, &directory.path().join("table-locks.db"));
        for schema in ["pg_catalog", "information_schema"] {
            let result = sql(&engine, &format!("SELECT r.rolname AS owner FROM pg_namespace n JOIN pg_roles r ON r.oid = n.nspowner WHERE n.nspname = '{schema}'"));
            assert_eq!(result.rows[0]["owner"], Value::Str("dependent".into()));
        }
    }
}
