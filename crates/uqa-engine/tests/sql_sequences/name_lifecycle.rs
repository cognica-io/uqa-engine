//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn scalar(engine: &Engine, sql: &str) -> Value {
    engine.sql(sql, &[]).unwrap().rows[0]
        .values()
        .next()
        .unwrap()
        .clone()
}

fn assert_sequence_name_lifecycle_transactions(engine: &Engine) {
    engine
        .sql(
            "CREATE SCHEMA archive; CREATE SEQUENCE transaction_ids CACHE 3",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("transaction_ids").unwrap(), 1);

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE transaction_ids RENAME TO transaction_renamed_ids",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("transaction_renamed_ids").unwrap(), 2);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(engine.currval("transaction_ids").unwrap(), 2);
    assert_eq!(engine.lastval().unwrap(), 2);
    assert_eq!(engine.nextval("transaction_ids").unwrap(), 3);

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("ALTER SEQUENCE transaction_ids SET SCHEMA archive", &[])
        .unwrap();
    assert_eq!(engine.nextval("archive.transaction_ids").unwrap(), 4);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(engine.currval("transaction_ids").unwrap(), 4);
    assert_eq!(engine.lastval().unwrap(), 4);
    assert_eq!(engine.nextval("transaction_ids").unwrap(), 5);

    engine
        .sql(
            "ALTER SEQUENCE transaction_ids RENAME TO committed_ids",
            &[],
        )
        .unwrap();
    engine
        .sql("ALTER SEQUENCE committed_ids SET SCHEMA archive", &[])
        .unwrap();
    assert_eq!(engine.nextval("archive.committed_ids").unwrap(), 6);

    engine.sql("BEGIN; SAVEPOINT lifecycle_point", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE archive.committed_ids RENAME TO savepoint_ids",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("archive.savepoint_ids").unwrap(), 7);
    engine
        .sql("ROLLBACK TO SAVEPOINT lifecycle_point; COMMIT", &[])
        .unwrap();
    assert_eq!(engine.currval("archive.committed_ids").unwrap(), 7);
    assert_eq!(engine.lastval().unwrap(), 7);
    assert_eq!(engine.nextval("archive.committed_ids").unwrap(), 8);

    engine
        .sql("CREATE SEQUENCE a_rekey_ids CACHE 3", &[])
        .unwrap();
    assert_eq!(engine.nextval("a_rekey_ids").unwrap(), 1);
    engine
        .sql("ALTER SEQUENCE a_rekey_ids RENAME TO z_rekey_ids", &[])
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    assert_eq!(engine.nextval("z_rekey_ids").unwrap(), 2);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(engine.currval("z_rekey_ids").unwrap(), 2);
}

#[test]
fn sequence_name_lifecycle_follows_transactions_in_memory() {
    assert_sequence_name_lifecycle_transactions(&Engine::new());
}

#[test]
fn sequence_name_lifecycle_follows_transactions_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sequence-name-lifecycle.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        assert_sequence_name_lifecycle_transactions(&engine);
    }
    let reopened = Engine::open(&database).unwrap();
    assert!(reopened.nextval("transaction_ids").is_err());
    assert!(reopened.currval("archive.committed_ids").is_err());
    assert_eq!(reopened.nextval("archive.committed_ids").unwrap(), 10);
}

#[test]
fn sequence_name_lifecycle_preserves_oid_cache_dependencies_and_errors() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SCHEMA archive;
             CREATE SEQUENCE ids CACHE 3;
             CREATE TABLE oid_holder(saved_oid oid);
             INSERT INTO oid_holder SELECT 'ids'::regclass::oid;
             CREATE TABLE dependent_rows(id bigint DEFAULT nextval('ids'), label text);
             CREATE VIEW dependent_view AS SELECT nextval('ids') AS id;
             CREATE TABLE serial_rows(id serial);
             CREATE TABLE identity_rows(id bigint GENERATED ALWAYS AS IDENTITY);
             CREATE SEQUENCE collision_ids;
             CREATE SEQUENCE archive.collision_ids;
             CREATE TABLE wrong_kind(id bigint);
             CREATE TABLE archive.archive_wrong_kind(id bigint)",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("ids").unwrap(), 1);
    let oid = scalar(&engine, "SELECT saved_oid FROM oid_holder");

    engine
        .sql("ALTER SEQUENCE ids RENAME TO renamed_ids", &[])
        .unwrap();
    assert_eq!(engine.nextval("renamed_ids").unwrap(), 2);
    assert_eq!(scalar(&engine, "SELECT 'renamed_ids'::regclass::oid"), oid);
    assert_eq!(
        scalar(
            &engine,
            "SELECT nextval(saved_oid::regclass) FROM oid_holder"
        ),
        Value::Int(3)
    );
    engine
        .sql("INSERT INTO dependent_rows(label) VALUES ('renamed')", &[])
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT id FROM dependent_rows"),
        Value::Int(4)
    );
    assert_eq!(
        scalar(&engine, "SELECT id FROM dependent_view"),
        Value::Int(5)
    );

    engine
        .sql("ALTER SEQUENCE renamed_ids SET SCHEMA archive", &[])
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT 'archive.renamed_ids'::regclass::oid"),
        oid
    );
    engine
        .sql("INSERT INTO dependent_rows(label) VALUES ('moved')", &[])
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT max(id) FROM dependent_rows"),
        Value::Int(6)
    );
    assert_eq!(
        scalar(&engine, "SELECT id FROM dependent_view"),
        Value::Int(7)
    );

    for sequence in ["serial_rows_id_seq", "identity_rows_id_seq"] {
        let renamed = format!("renamed_{sequence}");
        engine
            .sql(
                &format!("ALTER SEQUENCE {sequence} RENAME TO {renamed}"),
                &[],
            )
            .unwrap();
        let table = sequence.trim_end_matches("_id_seq");
        assert_eq!(
            scalar(
                &engine,
                &format!("SELECT pg_get_serial_sequence('{table}', 'id')")
            ),
            Value::Str(format!("public.{renamed}"))
        );
        engine
            .sql(&format!("INSERT INTO {table}(id) VALUES (DEFAULT)"), &[])
            .unwrap();
        assert_eq!(
            scalar(&engine, &format!("SELECT id FROM {table}")),
            Value::Int(1)
        );
        assert!(engine
            .sql(&format!("ALTER SEQUENCE {renamed} SET SCHEMA archive"), &[])
            .unwrap_err()
            .to_string()
            .contains("cannot move an owned sequence"));
    }

    assert_sequence_name_lifecycle_errors(&engine);
}

fn assert_sequence_name_lifecycle_errors(engine: &Engine) {
    for (sql, state) in [
        ("ALTER SEQUENCE missing_ids RENAME TO other_ids", "42P01"),
        ("ALTER SEQUENCE wrong_kind RENAME TO other_ids", "42809"),
        (
            "ALTER SEQUENCE archive.renamed_ids RENAME TO archive_wrong_kind",
            "42P07",
        ),
        (
            "ALTER SEQUENCE archive.renamed_ids SET SCHEMA missing_schema",
            "3F000",
        ),
        ("ALTER SEQUENCE collision_ids SET SCHEMA archive", "42P07"),
        (
            "ALTER SEQUENCE archive.renamed_ids RENAME TO renamed_ids",
            "42P07",
        ),
        ("ALTER SEQUENCE wrong_kind SET SCHEMA archive", "42809"),
    ] {
        assert_eq!(
            engine.sql(sql, &[]).unwrap_err().sqlstate(),
            Some(state),
            "{sql}"
        );
    }
}

#[test]
fn sequence_name_lifecycle_persists_oid_and_dependencies_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sequence-name-dependencies.sqlite");
    let oid;
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE SCHEMA archive;
                 CREATE SEQUENCE dependency_ids;
                 CREATE TABLE dependency_oid(saved_oid oid);
                 INSERT INTO dependency_oid SELECT 'dependency_ids'::regclass::oid;
                 CREATE TABLE dependency_rows(id bigint DEFAULT nextval('dependency_ids'));
                 CREATE VIEW dependency_view AS SELECT nextval('dependency_ids') AS id;
                 ALTER SEQUENCE dependency_ids RENAME TO renamed_dependency_ids;
                 ALTER SEQUENCE renamed_dependency_ids SET SCHEMA archive",
                &[],
            )
            .unwrap();
        oid = scalar(&engine, "SELECT saved_oid FROM dependency_oid");
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT 'archive.renamed_dependency_ids'::regclass::oid"
        ),
        oid
    );
    reopened
        .sql("INSERT INTO dependency_rows VALUES (DEFAULT)", &[])
        .unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT id FROM dependency_rows"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(&reopened, "SELECT id FROM dependency_view"),
        Value::Int(2)
    );
}

#[test]
fn sequence_name_lifecycle_supports_historical_temp_notice_and_read_only_paths() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SCHEMA archive;
             CREATE SEQUENCE historical_ids CACHE 3;
             CREATE TEMP SEQUENCE temporary_ids CACHE 3",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("historical_ids").unwrap(), 1);
    engine
        .sql(
            "ALTER TABLE historical_ids RENAME TO historical_renamed_ids",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("historical_renamed_ids").unwrap(), 2);
    engine
        .sql("ALTER TABLE historical_renamed_ids SET SCHEMA archive", &[])
        .unwrap();
    assert_eq!(engine.nextval("archive.historical_renamed_ids").unwrap(), 3);

    assert_eq!(engine.nextval("temporary_ids").unwrap(), 1);
    engine
        .sql(
            "ALTER SEQUENCE temporary_ids RENAME TO temporary_renamed_ids",
            &[],
        )
        .unwrap();
    assert_eq!(engine.nextval("temporary_renamed_ids").unwrap(), 2);
    assert_eq!(
        engine
            .sql(
                "ALTER SEQUENCE temporary_renamed_ids SET SCHEMA pg_temp",
                &[]
            )
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );

    engine
        .sql(
            "ALTER SEQUENCE IF EXISTS missing_ids RENAME TO renamed_missing_ids",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER SEQUENCE IF EXISTS missing_ids SET SCHEMA archive",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![
            (
                "NOTICE".to_string(),
                "relation \"missing_ids\" does not exist, skipping".to_string()
            ),
            (
                "NOTICE".to_string(),
                "relation \"missing_ids\" does not exist, skipping".to_string()
            )
        ]
    );

    for sql in [
        "ALTER SEQUENCE missing_ids RENAME TO renamed_missing_ids",
        "ALTER SEQUENCE missing_ids SET SCHEMA archive",
        "ALTER SEQUENCE temporary_renamed_ids RENAME TO temporary_again",
    ] {
        engine.sql("BEGIN READ ONLY", &[]).unwrap();
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some("25006"));
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn declared_cursor_keeps_sequence_object_identity_across_rename() {
    let engine = Engine::new();
    engine
        .sql("CREATE SEQUENCE cursor_ids CACHE 3; BEGIN", &[])
        .unwrap();
    engine
        .sql(
            "DECLARE lifecycle_cursor CURSOR FOR SELECT nextval('cursor_ids'::regclass) AS value",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER SEQUENCE cursor_ids RENAME TO cursor_renamed_ids",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("FETCH NEXT FROM lifecycle_cursor", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(1)
    );
    engine.sql("COMMIT", &[]).unwrap();
}
