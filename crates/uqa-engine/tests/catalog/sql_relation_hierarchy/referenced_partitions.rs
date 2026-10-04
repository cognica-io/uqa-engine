//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A partition of a referenced partitioned table holds referenced rows, which `TRUNCATE` and `DROP TABLE` of the partition must not leave dangling, as `PostgreSQL`'s derived constraint on each referenced partition prevents.

use super::{exec, Engine, Value};

fn fixture(engine: &Engine) {
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY, b text) PARTITION BY RANGE (a)",
        "CREATE TABLE pk1 PARTITION OF pk FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE pk2 PARTITION OF pk FOR VALUES FROM (10) TO (20)",
        "CREATE TABLE fk (a integer REFERENCES pk, c integer)",
        "INSERT INTO pk VALUES (1, 'one'), (2, 'two'), (12, 'twelve')",
        "INSERT INTO fk VALUES (1, 0), (12, 0)",
    ] {
        exec(engine, sql);
    }
}

fn count(engine: &Engine, sql: &str) -> Value {
    engine.sql(sql, &[]).unwrap().rows[0]
        .values()
        .next()
        .unwrap()
        .clone()
}

fn diagnostic(error: uqa_sql::SQLError) -> (String, String, Option<String>, Option<String>) {
    match error {
        uqa_sql::SQLError::Diagnostic {
            sqlstate,
            message,
            detail,
            hint,
        } => (sqlstate, message, detail, hint),
        other => panic!("expected diagnostic fields: {other:?}"),
    }
}

#[test]
fn truncating_a_referenced_partition_is_rejected_unless_it_cascades() {
    let engine = Engine::new();
    fixture(&engine);
    assert_eq!(
        diagnostic(engine.sql("TRUNCATE pk1", &[]).unwrap_err()),
        (
            "0A000".into(),
            "cannot truncate a table referenced in a foreign key constraint".into(),
            Some("Table \"fk\" references \"pk1\".".into()),
            Some("Truncate table \"fk\" at the same time, or use TRUNCATE ... CASCADE.".into())
        )
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM pk1"), Value::Int(2));
    exec(&engine, "TRUNCATE pk1, fk");
    assert_eq!(count(&engine, "SELECT count(*) FROM fk"), Value::Int(0));
    exec(&engine, "INSERT INTO fk VALUES (12, 1)");
    engine.take_sql_notices();
    exec(&engine, "TRUNCATE pk2 CASCADE");
    assert_eq!(
        engine.take_sql_notices(),
        [uqa_engine::SQLNotice::notice(
            "truncate cascades to table \"fk\""
        )]
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM fk"), Value::Int(0));
    assert_eq!(count(&engine, "SELECT count(*) FROM pk"), Value::Int(0));
}

#[test]
fn dropping_a_referenced_partition_requires_cascade_which_drops_the_foreign_key() {
    let engine = Engine::new();
    fixture(&engine);
    let error = engine.sql("DROP TABLE pk1", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    assert_eq!(count(&engine, "SELECT count(*) FROM pk1"), Value::Int(2));
    exec(&engine, "DROP TABLE pk1 CASCADE");
    // The whole foreign key goes, as PostgreSQL drops the constraint the partition's derived constraint belongs to; the referencing rows stay.
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM pg_constraint WHERE contype = 'f'"
        ),
        Value::Int(0)
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM fk"), Value::Int(2));
    exec(&engine, "INSERT INTO fk VALUES (5, 0)");
    assert_eq!(count(&engine, "SELECT count(*) FROM pk"), Value::Int(1));
}

#[test]
fn dropping_a_partition_of_a_referenced_partition_or_of_a_self_reference_is_rejected() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY) PARTITION BY RANGE (a)",
        "CREATE TABLE pk2 PARTITION OF pk FOR VALUES FROM (10) TO (20) PARTITION BY RANGE (a)",
        "CREATE TABLE pk21 PARTITION OF pk2 FOR VALUES FROM (10) TO (15)",
        "CREATE TABLE fk (a integer REFERENCES pk)",
        "CREATE TABLE tree (id integer PRIMARY KEY, parent integer REFERENCES tree) PARTITION BY RANGE (id)",
        "CREATE TABLE tree1 PARTITION OF tree FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE tree2 PARTITION OF tree FOR VALUES FROM (10) TO (20)",
    ] {
        exec(&engine, sql);
    }
    for sql in ["DROP TABLE pk21", "DROP TABLE pk2", "DROP TABLE tree1"] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("2BP01"), "{sql}: {error}");
    }
    // Dropping the whole referenced tree, or the referencing table with it, leaves nothing that references a dropped row.
    exec(&engine, "DROP TABLE pk, fk");
    exec(&engine, "DROP TABLE tree");
}

fn detach_error(engine: &Engine, sql: &str) -> (String, String, Option<String>, Option<String>) {
    diagnostic(engine.sql(sql, &[]).unwrap_err())
}

#[test]
fn detaching_a_referenced_partition_checks_the_rows_that_reference_its_subtree() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY) PARTITION BY RANGE (a)",
        "CREATE TABLE pk1 PARTITION OF pk FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE pk2 PARTITION OF pk FOR VALUES FROM (10) TO (20) PARTITION BY RANGE (a)",
        "CREATE TABLE pk21 PARTITION OF pk2 FOR VALUES FROM (10) TO (15)",
        "CREATE TABLE fk (a integer REFERENCES pk, b integer) PARTITION BY RANGE (b)",
        "CREATE TABLE fk1 PARTITION OF fk FOR VALUES FROM (0) TO (100)",
        "INSERT INTO pk VALUES (1), (2), (12)",
        "INSERT INTO fk VALUES (1, 5), (NULL, 6), (12, 7)",
    ] {
        exec(&engine, sql);
    }
    assert_eq!(
        detach_error(&engine, "ALTER TABLE pk DETACH PARTITION pk1"),
        (
            "23503".into(),
            "removing partition \"pk1\" violates foreign key constraint \"fk_a_fkey_1\"".into(),
            Some("Key (a)=(1) is still referenced from table \"fk\".".into()),
            None
        )
    );
    // A partitioned partition names its own derived constraint for a key its partitions hold.
    assert_eq!(
        detach_error(&engine, "ALTER TABLE pk DETACH PARTITION pk2"),
        (
            "23503".into(),
            "removing partition \"pk2\" violates foreign key constraint \"fk_a_fkey_2\"".into(),
            Some("Key (a)=(12) is still referenced from table \"fk\".".into()),
            None
        )
    );
    assert_eq!(
        detach_error(&engine, "ALTER TABLE pk2 DETACH PARTITION pk21").1,
        "removing partition \"pk21\" violates foreign key constraint \"fk_a_fkey_3\""
    );
    exec(&engine, "DELETE FROM fk WHERE a IS NOT NULL");
    exec(&engine, "ALTER TABLE pk DETACH PARTITION pk1");
    exec(&engine, "ALTER TABLE pk DETACH PARTITION pk2");
    let names = engine
        .sql(
            "SELECT conname FROM pg_constraint WHERE contype = 'f' AND conrelid = 'fk'::regclass",
            &[],
        )
        .unwrap()
        .rows
        .len();
    assert_eq!(names, 1);
}

#[test]
fn detaching_checks_foreign_keys_that_are_not_enforced_and_temporal_overlap() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE mpk (a integer, b integer, PRIMARY KEY (a, b)) PARTITION BY RANGE (a)",
        "CREATE TABLE mpk1 PARTITION OF mpk FOR VALUES FROM (0) TO (10)",
        "INSERT INTO mpk VALUES (1, 2)",
        "CREATE TABLE mfk (a integer, b integer, FOREIGN KEY (a, b) REFERENCES mpk NOT ENFORCED)",
        "INSERT INTO mfk VALUES (1, 2), (1, NULL), (5, 5)",
        "CREATE TABLE tpk (id integer, valid int4range, PRIMARY KEY (id, valid WITHOUT OVERLAPS)) PARTITION BY RANGE (id)",
        "CREATE TABLE tpk1 PARTITION OF tpk FOR VALUES FROM (0) TO (10)",
        "INSERT INTO tpk VALUES (1, '[1,10)')",
        "CREATE TABLE tfk (id integer, valid int4range, FOREIGN KEY (id, PERIOD valid) REFERENCES tpk (id, PERIOD valid) NOT ENFORCED)",
        "INSERT INTO tfk VALUES (1, '[8,12)')",
    ] {
        exec(&engine, sql);
    }
    assert_eq!(
        detach_error(&engine, "ALTER TABLE mpk DETACH PARTITION mpk1"),
        (
            "23503".into(),
            "removing partition \"mpk1\" violates foreign key constraint \"mfk_a_b_fkey_1\"".into(),
            Some("Key (a, b)=(1, 2) is still referenced from table \"mfk\".".into()),
            None
        )
    );
    // A temporal foreign key's row is referenced when its period overlaps a referenced row's.
    assert_eq!(
        detach_error(&engine, "ALTER TABLE tpk DETACH PARTITION tpk1"),
        (
            "23503".into(),
            "removing partition \"tpk1\" violates foreign key constraint \"tfk_id_valid_fkey_1\""
                .into(),
            Some("Key (id, valid)=(1, [8,12)) is still referenced from table \"tfk\".".into()),
            None
        )
    );
    exec(&engine, "UPDATE tfk SET valid = '[20,30)'");
    exec(&engine, "ALTER TABLE tpk DETACH PARTITION tpk1");
}
