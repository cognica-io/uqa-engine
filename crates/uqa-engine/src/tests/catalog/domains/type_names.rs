//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::relation_lock_support::after_shared_wait;
use uqa_execution::{
    row_locks::shared_objects::SharedCatalogLock,
    schema::namespaces::type_names::TYPE_CATALOG_CLASS_ID,
};

const DOMAIN: &str = "CREATE DOMAIN target AS int";
const ROW_TYPES: [&str; 4] = [
    "CREATE TABLE target(v int)",
    "CREATE VIEW target AS SELECT 1 AS v",
    "CREATE MATERIALIZED VIEW target AS SELECT 1 AS v",
    "CREATE FOREIGN TABLE target(v int) SERVER source",
];

fn reservation() -> SharedCatalogLock<'static> {
    SharedCatalogLock::Name {
        class_id: TYPE_CATALOG_CLASS_ID,
        name: "public.target",
    }
}

#[test]
fn concurrent_domains_preserve_the_winning_identity_and_fixed_data_snapshot() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_type; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(
                    &second,
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
                );
                sql(
                    &first,
                    &format!("BEGIN; SAVEPOINT before_type; {DOMAIN}; INSERT INTO t VALUES(2)"),
                );
                let first_id = first.durable.domains.read()["public.target"].object_id;
                let (second, result) =
                    after_shared_wait(&first, second, DOMAIN, reservation(), finish);
                if finish == "COMMIT" {
                    let error = result.unwrap_err();
                    assert_eq!(error.sqlstate(), Some("23505"));
                    assert!(error.to_string().contains("pg_type_typname_nsp_index"));
                    sql(&second, "ROLLBACK");
                    assert_eq!(
                        first.durable.domains.read()["public.target"].object_id,
                        first_id
                    );
                } else {
                    result.unwrap();
                    assert_ne!(
                        second.durable.domains.read()["public.target"].object_id,
                        first_id
                    );
                    sql(&first, "INSERT INTO t VALUES(2)");
                    assert_eq!(
                        sql(&second, "SELECT count(*) AS n FROM t").rows[0]["n"],
                        Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
                    );
                    sql(&second, "COMMIT");
                }
                drop((first, second));
                let engine = reopen(provider, &directory.path().join("table-locks.db"));
                sql(&engine, "SELECT 1::target");
                if finish == "COMMIT" {
                    assert_eq!(
                        engine.durable.domains.read()["public.target"].object_id,
                        first_id
                    );
                }
            }
        }
    }
}

#[test]
fn domains_and_row_types_share_a_destination_in_both_creation_orders() {
    for provider in 0..3 {
        for relation in ROW_TYPES {
            for (holder, worker) in [(DOMAIN, relation), (relation, DOMAIN)] {
                for finish in ["COMMIT", "ROLLBACK"] {
                    let (directory, first, second) = sessions(provider);
                    sql(
                        &first,
                        "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw",
                    );
                    sql(&first, &format!("BEGIN; {holder}"));
                    let (second, result) =
                        after_shared_wait(&first, second, worker, reservation(), finish);
                    if finish == "COMMIT" {
                        assert_eq!(
                            result.unwrap_err().sqlstate(),
                            Some("23505"),
                            "{provider}/{holder}/{worker}"
                        );
                    } else {
                        result.unwrap();
                    }
                    drop((first, second));
                    let engine = reopen(provider, &directory.path().join("table-locks.db"));
                    let winner = if finish == "COMMIT" { holder } else { worker };
                    assert_eq!(
                        engine.durable.domains.read().contains_key("public.target"),
                        winner == DOMAIN
                    );
                    error(&engine, DOMAIN, "42710");
                }
            }
        }
    }
}

#[test]
fn visible_domain_and_row_type_collisions_fail_before_destination_waits() {
    for provider in 0..3 {
        for relation in ROW_TYPES {
            for (first, second) in [(DOMAIN, relation), (relation, DOMAIN)] {
                let (_directory, engine, _peer) = sessions(provider);
                sql(
                    &engine,
                    "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw",
                );
                sql(&engine, first);
                error(&engine, second, "42710");
            }
        }
    }
}

#[test]
fn sequences_and_indexes_can_commit_under_a_reserved_domain_name() {
    for provider in 0..3 {
        for relation in ["CREATE SEQUENCE target", "CREATE INDEX target ON t(v)"] {
            for (holder, worker) in [(DOMAIN, relation), (relation, DOMAIN)] {
                let (directory, first, second) = sessions(provider);
                sql(&first, &format!("BEGIN; {holder}"));
                let second = before_commit(&first, second, worker);
                sql(&first, "SELECT 1::target");
                assert!(first.relation_kind_at("public.target").unwrap().is_some());
                drop((first, second));
                let engine = reopen(provider, &directory.path().join("table-locks.db"));
                sql(&engine, "SELECT 1::target");
                assert!(engine.relation_kind_at("public.target").unwrap().is_some());
            }
        }
    }
}

#[test]
fn row_type_renames_reserve_the_same_destination_as_domains() {
    for provider in 0..3 {
        for (create, rename) in [
            (
                "CREATE TABLE original(v int)",
                "ALTER TABLE original RENAME TO target",
            ),
            (
                "CREATE VIEW original AS SELECT 1 AS v",
                "ALTER VIEW original RENAME TO target",
            ),
            (
                "CREATE MATERIALIZED VIEW original AS SELECT 1 AS v",
                "ALTER MATERIALIZED VIEW original RENAME TO target",
            ),
            (
                "CREATE FOREIGN TABLE original(v int) SERVER source",
                "ALTER FOREIGN TABLE original RENAME TO target",
            ),
        ] {
            for (holder, worker) in [(DOMAIN, rename), (rename, DOMAIN)] {
                let (_directory, first, second) = sessions(provider);
                sql(
                    &first,
                    "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw",
                );
                sql(&first, create);
                sql(&first, &format!("BEGIN; {holder}"));
                let (_, result) =
                    after_shared_wait(&first, second, worker, reservation(), "COMMIT");
                assert_eq!(
                    result.unwrap_err().sqlstate(),
                    Some("23505"),
                    "{holder}/{worker}"
                );
            }
        }
    }
}
