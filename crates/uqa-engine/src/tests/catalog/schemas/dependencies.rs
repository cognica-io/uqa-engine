//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation keeps its namespace alive and rechecks names and authority after waits.

use crate::tests::relation_lock_support::{after_shared_wait, sessions, sql};
use crate::Engine;
use uqa_execution::{
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
    schema::namespaces::identity::SCHEMA_CATALOG_CLASS_ID,
};

fn schema_lock(engine: &Engine) -> SharedCatalogLock<'static> {
    SharedCatalogLock::Object {
        class_id: SCHEMA_CATALOG_CLASS_ID,
        oid: engine.durable.schemas.read()["s"].tuple.unwrap().oid as u32,
    }
}

#[test]
fn relation_creation_blocks_schema_removal_until_commit_or_undo() {
    for provider in 0..3 {
        for create in [
            "CREATE TABLE s.child(id integer)",
            "CREATE TABLE s.child AS SELECT 1 AS id",
            "CREATE VIEW s.child AS SELECT 1 AS id",
            "CREATE MATERIALIZED VIEW s.child AS SELECT 1 AS id",
            "CREATE SEQUENCE s.child",
            "CREATE FOREIGN TABLE s.child(id integer) SERVER remote",
        ] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_create"] {
                let (_directory, first, second) = sessions(provider);
                sql(
                    &first,
                    "CREATE SCHEMA s; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw",
                );
                let target = schema_lock(&first);
                sql(&first, &format!("BEGIN; SAVEPOINT before_create; {create}"));
                let (_, result) =
                    after_shared_wait(&first, second, "DROP SCHEMA s CASCADE", target, finish);
                result.unwrap_or_else(|error| panic!("{provider}/{create}/{finish}: {error}"));
                if finish.starts_with("ROLLBACK TO") {
                    sql(&first, "ROLLBACK");
                }
                assert!(!first.new_session().unwrap().has_schema("s").unwrap());
            }
        }
    }
}

#[test]
fn schema_restrict_checks_new_relations_after_the_creator_commits() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE SCHEMA s");
            let target = schema_lock(&first);
            sql(&first, "BEGIN; CREATE TABLE s.child(id integer)");
            sql(
                &second,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            let (second, result) =
                after_shared_wait(&first, second, "DROP SCHEMA s", target, "COMMIT");
            let error = result.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some("2BP01"),
                "{provider}/{isolation}: {error}"
            );
            sql(&second, "ROLLBACK");
            sql(&second, "SELECT * FROM s.child");
        }
    }
}

#[test]
fn relation_creation_rebinds_schema_deletion_recreation_and_rollback() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for (holder, finish, missing) in [
                ("DROP SCHEMA s", "COMMIT", true),
                ("DROP SCHEMA s; CREATE SCHEMA s", "COMMIT", false),
                ("DROP SCHEMA s", "ROLLBACK", false),
                ("DROP SCHEMA s", "ROLLBACK TO before_drop", false),
            ] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE SCHEMA s");
                let target = schema_lock(&first);
                sql(&first, &format!("BEGIN; SAVEPOINT before_drop; {holder}"));
                sql(
                    &second,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
                );
                let (second, result) = after_shared_wait(
                    &first,
                    second,
                    "CREATE TABLE s.child(id integer)",
                    target,
                    finish,
                );
                if missing {
                    let error = result.unwrap_err();
                    assert_eq!(
                        error.sqlstate(),
                        Some("3F000"),
                        "{provider}/{isolation}/{holder}: {error}"
                    );
                    sql(&second, "ROLLBACK");
                } else {
                    result.unwrap_or_else(|error| {
                        panic!("{provider}/{isolation}/{holder}/{finish}: {error}")
                    });
                    sql(&second, "COMMIT");
                    sql(&second, "SELECT * FROM s.child");
                }
                if finish.starts_with("ROLLBACK TO") {
                    sql(&first, "ROLLBACK");
                }
            }
        }
    }
}

#[test]
fn creation_rechecks_privileges_and_search_path_after_schema_waits() {
    for provider in 0..3 {
        for fallback in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE SCHEMA s; CREATE SCHEMA fallback; GRANT USAGE, CREATE ON SCHEMA s, fallback TO reader");
            let target = schema_lock(&first);
            sql(&second, "SET ROLE reader; SET search_path = s, fallback");
            sql(
                &first,
                if fallback {
                    "BEGIN; DROP SCHEMA s"
                } else {
                    "BEGIN; DROP SCHEMA s; CREATE SCHEMA s"
                },
            );
            let create = if fallback {
                "CREATE TABLE child(id integer)"
            } else {
                "CREATE TABLE s.child(id integer)"
            };
            let (second, result) = after_shared_wait(&first, second, create, target, "COMMIT");
            if fallback {
                result.unwrap();
                sql(&second, "SELECT * FROM fallback.child");
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some("42501"), "{error}");
            }
        }
    }
}

#[test]
fn unauthorized_creation_fails_before_waiting_for_schema_deletion() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE reader; CREATE SCHEMA s");
        sql(&second, "SET ROLE reader");
        sql(&first, "BEGIN; DROP SCHEMA s");
        let cancellation = second.runtime.cancellation.clone();
        let (send, done) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            let result = second.sql("CREATE TABLE s.child(id integer)", &[]);
            let _ = send.send(result);
        });
        let result = done.recv_timeout(std::time::Duration::from_secs(30));
        if result.is_err() {
            cancellation.cancel();
        }
        sql(&first, "COMMIT");
        task.join().unwrap();
        let error = result
            .expect("unauthorized creation must finish before DROP commits")
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"), "{provider}: {error}");
    }
}

#[test]
fn skipped_and_direct_table_creation_retain_namespace_dependencies() {
    for provider in 0..3 {
        for direct in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE SCHEMA s; CREATE TABLE s.existing(id integer)",
            );
            let target = schema_lock(&first);
            sql(&first, "BEGIN");
            if direct {
                first
                    .create_table(
                        "s.child",
                        uqa_analysis::standard_analyzer("english"),
                        Vec::new(),
                    )
                    .unwrap();
            } else {
                sql(&first, "CREATE TABLE IF NOT EXISTS s.existing(id integer)");
            }
            let (_, result) =
                after_shared_wait(&first, second, "DROP SCHEMA s CASCADE", target, "COMMIT");
            result.unwrap();
            assert!(!first.new_session().unwrap().has_schema("s").unwrap());
        }
    }
}

#[test]
fn routine_and_domain_creation_do_not_acquire_relation_namespace_locks() {
    for provider in 0..3 {
        for create in [
            "CREATE FUNCTION s.child() RETURNS integer LANGUAGE SQL AS 'SELECT 1'",
            "CREATE DOMAIN s.child AS integer",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE SCHEMA s");
            let target = schema_lock(&first);
            sql(&first, &format!("BEGIN; {create}"));
            assert!(
                first
                    .row_locks
                    .try_acquire_relation(
                        second.session_id,
                        first.row_locks.shared_catalog_key(target),
                        RelationLockMode::AccessExclusive,
                        0,
                        &second.runtime.cancellation,
                    )
                    .unwrap(),
                "{provider}/{create}"
            );
            first.row_locks.release_session(second.session_id);
            sql(&first, "ROLLBACK");
        }
    }
}

#[test]
fn skipped_query_creation_does_not_retain_a_namespace_lock() {
    for provider in 0..3 {
        for create in [
            "CREATE TABLE IF NOT EXISTS s.existing AS SELECT 1 AS id",
            "CREATE MATERIALIZED VIEW IF NOT EXISTS s.existing AS SELECT 1 AS id",
        ] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                "CREATE SCHEMA s; CREATE TABLE s.existing(id integer)",
            );
            let target = schema_lock(&first);
            sql(&first, &format!("BEGIN; {create}"));
            assert!(
                first
                    .row_locks
                    .try_acquire_relation(
                        second.session_id,
                        first.row_locks.shared_catalog_key(target),
                        RelationLockMode::AccessExclusive,
                        0,
                        &second.runtime.cancellation,
                    )
                    .unwrap(),
                "{provider}/{create}"
            );
            first.row_locks.release_session(second.session_id);
            sql(&second, "DROP SCHEMA s CASCADE");
            sql(&first, "COMMIT");
        }
    }
}
