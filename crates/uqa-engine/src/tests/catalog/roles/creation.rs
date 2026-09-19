//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! New object owners retain their original role through dependency waits and transaction undo.

use super::{coordination::after_wait, identity::reopen};
use crate::{
    tests::relation_lock_support::{sessions, sql},
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

struct Target {
    create: &'static str,
    catalog: &'static str,
    name_column: &'static str,
    owner_column: &'static str,
    temporary: bool,
}

const TARGETS: &[Target] = &[
    Target {
        create: "CREATE DOMAIN created AS integer",
        catalog: "pg_type",
        name_column: "typname",
        owner_column: "typowner",
        temporary: false,
    },
    Target {
        create: "CREATE TABLE created(v int)",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: false,
    },
    Target {
        create: "CREATE TABLE created(id serial)",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: false,
    },
    Target {
        create: "CREATE TABLE created AS SELECT 1 AS v",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: false,
    },
    Target {
        create: "CREATE VIEW created AS SELECT 1 AS v",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: false,
    },
    Target {
        create: "CREATE MATERIALIZED VIEW created AS SELECT 1 AS v",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: false,
    },
    Target {
        create: "CREATE FOREIGN TABLE created(v int) SERVER remote",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: false,
    },
    Target {
        create: "CREATE SEQUENCE created",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: false,
    },
    Target {
        create: "CREATE SCHEMA created",
        catalog: "pg_namespace",
        name_column: "nspname",
        owner_column: "nspowner",
        temporary: false,
    },
    Target {
        create: "CREATE FUNCTION created() RETURNS int LANGUAGE SQL AS 'SELECT 1'",
        catalog: "pg_proc",
        name_column: "proname",
        owner_column: "proowner",
        temporary: false,
    },
    Target {
        create: "CREATE PROCEDURE created() LANGUAGE SQL AS 'SELECT 1'",
        catalog: "pg_proc",
        name_column: "proname",
        owner_column: "proowner",
        temporary: false,
    },
    Target {
        create: "CREATE TEMP TABLE created(v int)",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: true,
    },
    Target {
        create: "CREATE TEMP SEQUENCE created",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: true,
    },
    Target {
        create: "CREATE TEMP VIEW created AS SELECT 1 AS v",
        catalog: "pg_class",
        name_column: "relname",
        owner_column: "relowner",
        temporary: true,
    },
];

fn setup(engine: &Engine) -> SharedCatalogLock<'static> {
    sql(engine, "CREATE ROLE dependent; GRANT CREATE ON DATABASE uqa TO PUBLIC; GRANT CREATE ON SCHEMA public TO PUBLIC; GRANT INSERT ON t TO PUBLIC; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw");
    SharedCatalogLock::Object {
        class_id: ROLE_CATALOG_CLASS_ID,
        oid: u32::try_from(engine.durable.roles.read()["dependent"].oid).unwrap(),
    }
}

impl Target {
    fn assert_created(&self, engine: &Engine, created: bool) {
        self.assert_created_as(engine, created, "dependent");
    }

    fn assert_created_as(&self, engine: &Engine, created: bool, owner: &str) {
        let result = sql(engine, &format!("SELECT r.rolname AS owner FROM {} c JOIN pg_roles r ON r.oid = c.{} WHERE c.{} = 'created'", self.catalog, self.owner_column, self.name_column));
        assert_eq!(result.rows.len(), usize::from(created), "{}", self.create);
        if created {
            assert_eq!(result.rows[0]["owner"], Value::Str(owner.into()));
        }
    }
}

#[test]
fn creation_waits_retain_owners_through_rename_and_name_reuse() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for target in TARGETS {
                let (directory, first, second) = sessions(provider);
                let lock = setup(&first);
                let original = first.durable.roles.read()["dependent"].identity();
                sql(&first, "BEGIN");
                first
                    .acquire_shared_catalog(lock, RelationLockMode::AccessExclusive)
                    .unwrap()
                    .retain();
                sql(
                    &second,
                    &format!("SET ROLE dependent; BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                );
                let (second, result) = after_wait(
                    &first,
                    second,
                    target.create,
                    lock,
                    "ALTER ROLE dependent RENAME TO renamed; CREATE ROLE dependent; COMMIT",
                );
                result.unwrap();
                sql(&second, "COMMIT; RESET ROLE");
                target.assert_created_as(&second, true, "renamed");
                if target.create.contains("serial") {
                    assert_eq!(
                        sql(
                            &second,
                            "SELECT relowner FROM pg_class WHERE relname='created_id_seq'"
                        )
                        .rows[0]["relowner"],
                        Value::Int(original.oid)
                    );
                }
                drop(second);
                drop(first);
                target.assert_created_as(
                    &reopen(provider, &directory.path().join("table-locks.db")),
                    !target.temporary,
                    "renamed",
                );
            }
        }
    }
}

#[test]
fn role_deletion_waits_for_creation_and_observes_commit_or_undo() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for target in TARGETS {
                for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT"] {
                    let (directory, first, second) = sessions(provider);
                    let lock = setup(&first);
                    sql(&first, &format!("SET ROLE dependent; BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2); SAVEPOINT undo; {}", target.create));
                    sql(
                        &second,
                        &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT 1"),
                    );
                    let (second, result) =
                        after_wait(&first, second, "DROP ROLE dependent", lock, finish);
                    let created = finish == "COMMIT";
                    if created {
                        assert_eq!(
                            result.unwrap_err().sqlstate(),
                            Some("2BP01"),
                            "{} {provider} {isolation}",
                            target.create
                        );
                        sql(&second, "ROLLBACK");
                    } else {
                        result.unwrap();
                        sql(&second, "COMMIT");
                    }
                    sql(&first, "RESET ROLE");
                    target.assert_created(&first, created);
                    assert_eq!(
                        sql(&first, "SELECT v FROM t").rows.len(),
                        if finish == "ROLLBACK" { 1 } else { 2 }
                    );
                    drop(second);
                    drop(first);
                    target.assert_created(
                        &reopen(provider, &directory.path().join("table-locks.db")),
                        created && !target.temporary,
                    );
                }
            }
        }
    }
}

#[test]
fn creation_waits_for_role_deletion_without_adopting_a_recreated_name() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for target in TARGETS {
                for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT", "RECREATE"] {
                    let (directory, first, second) = sessions(provider);
                    let lock = setup(&first);
                    sql(&second, &format!("SET ROLE dependent; BEGIN ISOLATION LEVEL {isolation}; INSERT INTO t VALUES (2)"));
                    sql(&first, "BEGIN; SAVEPOINT undo; DROP ROLE dependent");
                    if finish == "RECREATE" {
                        sql(&first, "CREATE ROLE dependent");
                    }
                    let (second, result) = after_wait(
                        &first,
                        second,
                        target.create,
                        lock,
                        if finish == "RECREATE" {
                            "COMMIT"
                        } else {
                            finish
                        },
                    );
                    let created = !matches!(finish, "COMMIT" | "RECREATE");
                    if created {
                        result.unwrap();
                        sql(&second, "COMMIT; RESET ROLE");
                    } else {
                        assert_eq!(
                            result.unwrap_err().sqlstate(),
                            Some("42704"),
                            "{} {provider} {isolation}",
                            target.create
                        );
                        sql(&second, "ROLLBACK; RESET ROLE");
                    }
                    target.assert_created(&second, created);
                    assert_eq!(
                        sql(&first, "SELECT v FROM t").rows.len(),
                        if created { 2 } else { 1 }
                    );
                    drop(second);
                    drop(first);
                    target.assert_created(
                        &reopen(provider, &directory.path().join("table-locks.db")),
                        created && !target.temporary,
                    );
                }
            }
        }
    }
}

#[test]
fn a_committed_temporary_object_keeps_its_owner_dependency_visible_to_other_sessions() {
    for provider in 0..3 {
        for target in TARGETS.iter().filter(|target| target.temporary) {
            let (_directory, first, second) = sessions(provider);
            setup(&first);
            sql(
                &first,
                &format!("SET ROLE dependent; {}; RESET ROLE", target.create),
            );
            let error = second
                .sql("DROP ROLE dependent", &[])
                .expect_err(target.create);
            assert_eq!(error.sqlstate(), Some("2BP01"));
            drop(first);
            sql(&second, "DROP ROLE dependent");
        }
    }
}

#[test]
fn temporary_acl_changes_and_undo_update_shared_dependencies() {
    for provider in 0..3 {
        for (create, grant, revoke, drop) in [
            (
                "CREATE TEMP TABLE created(v int)",
                "GRANT SELECT(v) ON created TO dependent",
                "REVOKE SELECT(v) ON created FROM dependent",
                "DROP TABLE created",
            ),
            (
                "CREATE TEMP VIEW created AS SELECT 1 AS v",
                "GRANT SELECT ON created TO dependent",
                "REVOKE SELECT ON created FROM dependent",
                "DROP VIEW created",
            ),
            (
                "CREATE TEMP SEQUENCE created",
                "GRANT USAGE ON SEQUENCE created TO dependent",
                "REVOKE USAGE ON SEQUENCE created FROM dependent",
                "DROP SEQUENCE created",
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            setup(&first);
            sql(&first, &format!("{create}; {grant}"));
            assert_eq!(
                second
                    .sql("DROP ROLE dependent", &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("2BP01")
            );
            sql(&first, &format!("BEGIN; {revoke}; ROLLBACK"));
            assert_eq!(
                second
                    .sql("DROP ROLE dependent", &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("2BP01")
            );
            sql(
                &first,
                &format!("BEGIN; SAVEPOINT undo; {drop}; ROLLBACK TO undo; COMMIT"),
            );
            assert_eq!(
                second
                    .sql("DROP ROLE dependent", &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("2BP01")
            );
            sql(&first, revoke);
            sql(&second, "DROP ROLE dependent");
        }
    }
}

#[test]
fn temporary_owner_transfer_and_object_discard_release_committed_dependencies() {
    for provider in 0..3 {
        for (create, alter) in [
            (
                "CREATE TEMP TABLE created(v int)",
                "ALTER TABLE created OWNER TO uqa",
            ),
            (
                "CREATE TEMP VIEW created AS SELECT 1 AS v",
                "ALTER VIEW created OWNER TO uqa",
            ),
            (
                "CREATE TEMP SEQUENCE created",
                "ALTER SEQUENCE created OWNER TO uqa",
            ),
        ] {
            for finish in [alter, "DISCARD TEMP", "DISCARD ALL"] {
                let (_directory, first, second) = sessions(provider);
                setup(&first);
                sql(&first, &format!("SET ROLE dependent; {create}; RESET ROLE"));
                sql(&first, &format!("BEGIN; {alter}; ROLLBACK"));
                sql(&first, "BEGIN; DISCARD TEMP; ROLLBACK");
                sql(
                    &first,
                    "BEGIN; SAVEPOINT undo; DISCARD TEMP; ROLLBACK TO undo; COMMIT",
                );
                assert_eq!(
                    second
                        .sql("DROP ROLE dependent", &[])
                        .unwrap_err()
                        .sqlstate(),
                    Some("2BP01")
                );
                sql(&first, finish);
                second
                    .sql("DROP ROLE dependent", &[])
                    .unwrap_or_else(|error| panic!("{provider}: {create}; {finish}: {error}"));
            }
        }
        let (_directory, first, second) = sessions(provider);
        setup(&first);
        sql(&first, "SET ROLE dependent; BEGIN; CREATE TEMP TABLE created(v int) ON COMMIT DROP; COMMIT; RESET ROLE");
        sql(&second, "DROP ROLE dependent");
    }
}

#[test]
fn skipped_creation_and_replacement_do_not_add_a_dependency_on_the_invoking_role() {
    for provider in 0..3 {
        for (create, replace) in [
            (
                "CREATE TABLE created(v int)",
                "CREATE TABLE IF NOT EXISTS created(v int)",
            ),
            (
                "CREATE TABLE created(v int)",
                "CREATE TABLE IF NOT EXISTS created AS SELECT 1/0 AS v",
            ),
            (
                "CREATE SEQUENCE created",
                "CREATE SEQUENCE IF NOT EXISTS created",
            ),
            (
                "CREATE SCHEMA created",
                "CREATE SCHEMA IF NOT EXISTS created",
            ),
            (
                "CREATE MATERIALIZED VIEW created AS SELECT 1 AS v",
                "CREATE MATERIALIZED VIEW IF NOT EXISTS created AS SELECT 1/0 AS v",
            ),
            (
                "CREATE FOREIGN TABLE created(v int) SERVER remote",
                "CREATE FOREIGN TABLE IF NOT EXISTS created(v int) SERVER remote",
            ),
            (
                "CREATE VIEW created AS SELECT 1 AS v",
                "CREATE OR REPLACE VIEW created AS SELECT 2 AS v",
            ),
            (
                "CREATE FUNCTION created() RETURNS int LANGUAGE SQL AS 'SELECT 1'",
                "CREATE OR REPLACE FUNCTION created() RETURNS int LANGUAGE SQL AS 'SELECT 2'",
            ),
            (
                "CREATE PROCEDURE created() LANGUAGE SQL AS 'SELECT 1'",
                "CREATE OR REPLACE PROCEDURE created() LANGUAGE SQL AS 'SELECT 2'",
            ),
        ] {
            let (_directory, first, second) = sessions(provider);
            let lock = setup(&first);
            // Commit role setup before BEGIN so its pg_authid tuple lock cannot mask creation dependencies.
            sql(
                &first,
                &format!("ALTER ROLE dependent SUPERUSER; {create}; SET ROLE dependent"),
            );
            sql(&first, &format!("BEGIN; {replace}"));
            let SharedCatalogLock::Object { class_id, oid } = lock else {
                unreachable!()
            };
            for address in [lock, SharedCatalogLock::Tuple { class_id, oid }] {
                let key = first.row_locks.shared_catalog_key(address);
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
                    "{replace}: {address:?}"
                );
            }
            first.row_locks.release_session(second.session_id);
            sql(&second, "DROP ROLE dependent");
            sql(&first, "ROLLBACK; RESET ROLE");
        }
    }
}

#[test]
fn direct_table_and_schema_apis_retain_creation_owner_dependencies() {
    for provider in 0..3 {
        for schema in [false, true] {
            let (_directory, first, second) = sessions(provider);
            let lock = setup(&first);
            sql(&first, "SET ROLE dependent; BEGIN");
            if schema {
                first.register_schema("created", false).unwrap();
            } else {
                first
                    .create_table(
                        "created",
                        uqa_analysis::standard_analyzer("english"),
                        Vec::new(),
                    )
                    .unwrap();
            }
            let (second, result) =
                after_wait(&first, second, "DROP ROLE dependent", lock, "COMMIT");
            assert_eq!(result.unwrap_err().sqlstate(), Some("2BP01"));
            sql(&first, "RESET ROLE");
            sql(
                &first,
                if schema {
                    "DROP SCHEMA created"
                } else {
                    "DROP TABLE created"
                },
            );
            sql(&second, "DROP ROLE dependent");
        }
    }
}
