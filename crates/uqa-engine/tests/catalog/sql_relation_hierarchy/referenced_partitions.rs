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
