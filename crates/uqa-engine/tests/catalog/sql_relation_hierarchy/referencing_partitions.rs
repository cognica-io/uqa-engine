//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A foreign key declared on one partition of a partitioned referencing table acts on the rows of that partition alone, as `PostgreSQL` creates the action triggers of that partition's constraint, while a foreign key declared on the partitioned table acts on the rows of all its partitions.

use super::{exec, Engine, Value};

fn fixture(engine: &Engine, foreign_key: &str) {
    for sql in [
        "CREATE TABLE pk (a integer PRIMARY KEY)",
        "CREATE TABLE fk (a integer, b integer) PARTITION BY RANGE (b)",
        "CREATE TABLE fk1 PARTITION OF fk FOR VALUES FROM (0) TO (10)",
        "CREATE TABLE fk2 PARTITION OF fk FOR VALUES FROM (10) TO (20)",
        foreign_key,
        "INSERT INTO pk VALUES (1), (2), (3)",
        "INSERT INTO fk VALUES (1, 5), (1, 15), (2, 5), (2, 15)",
    ] {
        exec(engine, sql);
    }
}

fn rows(engine: &Engine) -> Vec<(Value, Value)> {
    engine
        .sql("SELECT a, b FROM fk ORDER BY b, a", &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| {
            let mut values = row.values();
            (
                values.next().unwrap().clone(),
                values.next().unwrap().clone(),
            )
        })
        .collect()
}

#[test]
fn a_partition_foreign_key_cascades_into_the_rows_of_its_partition_alone() {
    let engine = Engine::new();
    fixture(
        &engine,
        "ALTER TABLE fk1 ADD CONSTRAINT fk1_cascade FOREIGN KEY (a) REFERENCES pk ON DELETE CASCADE ON UPDATE CASCADE",
    );
    exec(&engine, "DELETE FROM pk WHERE a = 1");
    exec(&engine, "UPDATE pk SET a = 20 WHERE a = 2");
    assert_eq!(
        rows(&engine),
        vec![
            (Value::Int(20), Value::Int(5)),
            (Value::Int(1), Value::Int(15)),
            (Value::Int(2), Value::Int(15)),
        ]
    );
}

#[test]
fn a_partition_foreign_key_checks_the_rows_of_its_partition_alone() {
    let engine = Engine::new();
    fixture(
        &engine,
        "ALTER TABLE fk1 ADD CONSTRAINT fk1_no_action FOREIGN KEY (a) REFERENCES pk",
    );
    exec(&engine, "INSERT INTO fk VALUES (3, 15)");
    exec(&engine, "DELETE FROM pk WHERE a = 3");
    let error = engine.sql("DELETE FROM pk WHERE a = 1", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"), "{error}");
    assert!(
        error.to_string().contains(
            "update or delete on table \"pk\" violates foreign key constraint \"fk1_no_action\" on table \"fk1\""
        ),
        "{error}"
    );
}

#[test]
fn a_partitioned_table_foreign_key_cascades_into_the_rows_of_every_partition() {
    let engine = Engine::new();
    fixture(
        &engine,
        "ALTER TABLE fk ADD CONSTRAINT fk_cascade FOREIGN KEY (a) REFERENCES pk ON DELETE CASCADE",
    );
    exec(&engine, "DELETE FROM pk WHERE a = 1");
    assert_eq!(
        rows(&engine),
        vec![
            (Value::Int(2), Value::Int(5)),
            (Value::Int(2), Value::Int(15)),
        ]
    );
}
