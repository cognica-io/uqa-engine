//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One relation namespace coordinates independent creators and renamers.

use super::{error, sessions, sql, Value};
use crate::tests::relation_lock_support::after_shared_wait;
use uqa_execution::row_locks::shared_objects::SharedCatalogLock;

#[rstest::rstest]
#[case::index_renames(
    "CREATE INDEX one ON t(v); CREATE INDEX two ON t(v)",
    "ALTER INDEX one RENAME TO shared",
    "ALTER INDEX two RENAME TO shared"
)]
#[case::constraint_index_renames(
    "ALTER TABLE t ADD CONSTRAINT one UNIQUE(v); ALTER TABLE t ADD CONSTRAINT two UNIQUE(v)",
    "ALTER INDEX one RENAME TO shared",
    "ALTER INDEX two RENAME TO shared"
)]
#[case::create_index_then_rename(
    "CREATE INDEX one ON t(v)",
    "CREATE INDEX shared ON t(v)",
    "ALTER INDEX one RENAME TO shared"
)]
#[case::rename_then_create_index(
    "CREATE INDEX one ON t(v)",
    "ALTER INDEX one RENAME TO shared",
    "CREATE INDEX shared ON t(v)"
)]
#[case::create_table_then_rename_index(
    "CREATE INDEX one ON t(v)",
    "CREATE TABLE shared(v int)",
    "ALTER INDEX one RENAME TO shared"
)]
#[case::rename_index_then_create_table(
    "CREATE INDEX one ON t(v)",
    "ALTER INDEX one RENAME TO shared",
    "CREATE TABLE shared(v int)"
)]
#[case::table_renames(
    "CREATE TABLE one(v int); CREATE TABLE two(v int)",
    "ALTER TABLE one RENAME TO shared",
    "ALTER TABLE two RENAME TO shared"
)]
fn relation_name_destinations_wait_for_commit_or_savepoint_undo(
    #[values(0, 1, 2)] provider: usize,
    #[values("COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT")] release: &str,
    #[case] setup: &str,
    #[case] holder: &str,
    #[case] waiter: &str,
) {
    assert_destination(provider, release, setup, holder, waiter);
}

#[rstest::rstest]
#[case::views(
    "",
    "CREATE VIEW shared AS SELECT 1 AS v",
    "CREATE VIEW shared AS SELECT 2 AS v"
)]
#[case::materialized_views(
    "",
    "CREATE MATERIALIZED VIEW shared AS SELECT 1 AS v",
    "CREATE MATERIALIZED VIEW shared AS SELECT 2 AS v"
)]
#[case::sequence_renames(
    "CREATE SEQUENCE one; CREATE SEQUENCE two",
    "ALTER SEQUENCE one RENAME TO shared",
    "ALTER SEQUENCE two RENAME TO shared"
)]
#[case::create_sequence_then_rename_index(
    "CREATE INDEX one ON t(v)",
    "CREATE SEQUENCE shared",
    "ALTER INDEX one RENAME TO shared"
)]
#[case::rename_index_then_create_sequence(
    "CREATE INDEX one ON t(v)",
    "ALTER INDEX one RENAME TO shared",
    "CREATE SEQUENCE shared"
)]
#[case::create_foreign_table_then_rename_view(
    "CREATE VIEW one AS SELECT 1 AS v",
    "CREATE FOREIGN TABLE shared(v int) SERVER source",
    "ALTER VIEW one RENAME TO shared"
)]
#[case::rename_view_then_create_foreign_table(
    "CREATE VIEW one AS SELECT 1 AS v",
    "ALTER VIEW one RENAME TO shared",
    "CREATE FOREIGN TABLE shared(v int) SERVER source"
)]
#[case::rename_foreign_table_then_create_index(
    "CREATE FOREIGN TABLE one(v int) SERVER source",
    "ALTER FOREIGN TABLE one RENAME TO shared",
    "CREATE INDEX shared ON t(v)"
)]
#[case::add_constraint_then_rename_index(
    "CREATE TABLE a(v int); CREATE INDEX one ON t(v)",
    "ALTER TABLE a ADD CONSTRAINT shared UNIQUE(v)",
    "ALTER INDEX one RENAME TO shared"
)]
#[case::rename_index_then_add_constraint(
    "CREATE TABLE a(v int); CREATE INDEX one ON t(v)",
    "ALTER INDEX one RENAME TO shared",
    "ALTER TABLE a ADD CONSTRAINT shared UNIQUE(v)"
)]
#[case::rename_index_then_create_constraint(
    "CREATE INDEX one ON t(v)",
    "ALTER INDEX one RENAME TO shared",
    "CREATE TABLE a(v int CONSTRAINT shared UNIQUE)"
)]
#[case::existing_index(
    "CREATE INDEX one ON t(v)",
    "ALTER INDEX one RENAME TO shared",
    "CREATE INDEX IF NOT EXISTS shared ON t(v)"
)]
#[case::existing_table(
    "CREATE INDEX one ON t(v)",
    "ALTER INDEX one RENAME TO shared",
    "CREATE TABLE IF NOT EXISTS shared(v int)"
)]
#[case::create_table_as(
    "CREATE INDEX one ON t(v)",
    "ALTER INDEX one RENAME TO shared",
    "CREATE TABLE shared AS SELECT 1 AS v"
)]
fn relation_kinds_and_implicit_indexes_share_destination_reservations(
    #[values(0, 1, 2)] provider: usize,
    #[values("COMMIT", "ROLLBACK", "ROLLBACK TO undo; COMMIT")] release: &str,
    #[case] setup: &str,
    #[case] holder: &str,
    #[case] waiter: &str,
) {
    assert_destination(provider, release, setup, holder, waiter);
}

fn assert_destination(provider: usize, release: &str, setup: &str, holder: &str, waiter: &str) {
    let (_directory, first, second) = sessions(provider);
    sql(
        &first,
        "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw",
    );
    if !setup.is_empty() {
        sql(&first, setup);
    }
    sql(&first, &format!("BEGIN; SAVEPOINT undo; {holder}"));
    let (_second, result) = after_shared_wait(
        &first,
        second,
        waiter,
        SharedCatalogLock::Name {
            class_id: 1259,
            name: "public.shared",
        },
        release,
    );
    if release == "COMMIT" {
        assert_eq!(
            result.unwrap_err().sqlstate(),
            Some("23505"),
            "provider {provider}: {holder}; {waiter}"
        );
    } else {
        result.unwrap_or_else(|error| panic!("provider {provider}: {holder}; {waiter}: {error}"));
    }
}

#[test]
fn committed_source_name_conflicts_without_waiting_for_a_rename() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE INDEX one ON t(v)");
        sql(&first, "BEGIN; ALTER INDEX one RENAME TO shared");
        error(&second, "CREATE INDEX one ON t(v)", "42P07");
        sql(&first, "ROLLBACK");
    }
}

#[test]
fn name_waits_refresh_catalogs_without_advancing_fixed_data_snapshots() {
    for provider in 0..3 {
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            for release in ["COMMIT", "ROLLBACK"] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE INDEX one ON t(v); CREATE INDEX two ON t(v)");
                sql(
                    &second,
                    &format!(
                        "BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT attempt"
                    ),
                );
                sql(
                    &first,
                    "BEGIN; ALTER INDEX one RENAME TO shared; INSERT INTO t VALUES(2)",
                );
                let (second, result) = after_shared_wait(
                    &first,
                    second,
                    "ALTER INDEX two RENAME TO shared",
                    SharedCatalogLock::Name {
                        class_id: 1259,
                        name: "public.shared",
                    },
                    release,
                );
                if release == "COMMIT" {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("23505"));
                    sql(&second, "ROLLBACK TO attempt");
                } else {
                    result.unwrap();
                }
                assert_eq!(
                    sql(&second, "SELECT count(*) AS n FROM t").rows[0]["n"],
                    Value::Int(1)
                );
                assert_eq!(
                    sql(
                        &second,
                        "SELECT count(*) AS n FROM pg_class WHERE relname='shared'"
                    )
                    .rows[0]["n"],
                    Value::Int(1)
                );
                sql(&second, "COMMIT");
            }
        }
    }
}

#[test]
fn destination_reservations_distinguish_qualified_and_temporary_names() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE SCHEMA app; CREATE INDEX one ON t(v)");
        sql(
            &first,
            "BEGIN; ALTER INDEX one RENAME TO shared; CREATE TEMP TABLE local(v int)",
        );
        sql(&second, "CREATE TABLE app.shared(v int); CREATE TABLE \"app.shared\"(v int); CREATE TEMP TABLE local(v int)");
        sql(&first, "COMMIT");
        assert_eq!(
            sql(
                &second,
                "SELECT count(*) AS n FROM pg_class WHERE relname='shared'"
            )
            .rows[0]["n"],
            Value::Int(2)
        );
    }
}
