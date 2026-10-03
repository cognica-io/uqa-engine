//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A PRIMARY KEY or UNIQUE constraint of a partitioned table holds every partition key column, so that each partition's index enforces it alone, as `PostgreSQL`'s `DefineIndex` requires of the table and of every partition that builds an index for the key; a key added to a partitioned table also checks each partition's rows.

use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

/// The SQLSTATE, message and detail of the error `sql` fails with.
fn failure(engine: &Engine, sql: &str) -> (String, String, Option<String>) {
    let error = engine.sql(sql, &[]).unwrap_err();
    let detail = match &error {
        uqa_sql::SQLError::Diagnostic { detail, .. } => detail.clone(),
        _ => None,
    };
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
        detail,
    )
}

fn lacks(kind: &str, table: &str, column: &str) -> (String, String, Option<String>) {
    (
        "0A000".into(),
        "unique constraint on partitioned table must include all partitioning columns".into(),
        Some(format!(
            "{kind} constraint on table \"{table}\" lacks column \"{column}\" which is part of the partition key."
        )),
    )
}

#[test]
fn create_table_keys_hold_the_partition_key_columns() {
    let engine = Engine::new();
    for (sql, expected) in [
        (
            "CREATE TABLE tp (a int, b int, PRIMARY KEY (b)) PARTITION BY RANGE (a)",
            lacks("PRIMARY KEY", "tp", "a"),
        ),
        (
            "CREATE TABLE tu (a int, b int UNIQUE) PARTITION BY RANGE (a)",
            lacks("UNIQUE", "tu", "a"),
        ),
        (
            "CREATE TABLE ti (a int, b int, UNIQUE (b) INCLUDE (a)) PARTITION BY RANGE (a)",
            lacks("UNIQUE", "ti", "a"),
        ),
        (
            "CREATE TABLE tm (a int, b int, c int, UNIQUE (c, a)) PARTITION BY RANGE (b, a)",
            lacks("UNIQUE", "tm", "b"),
        ),
        // The primary key's index is built first.
        (
            "CREATE TABLE to1 (a int, b int UNIQUE, c int, PRIMARY KEY (c)) PARTITION BY RANGE (a)",
            lacks("PRIMARY KEY", "to1", "a"),
        ),
        // The partition key is checked before the index attributes.
        (
            "CREATE TABLE ts (a int, UNIQUE (ctid)) PARTITION BY RANGE (a)",
            lacks("UNIQUE", "ts", "a"),
        ),
        (
            "CREATE TABLE tv (a int, v int GENERATED ALWAYS AS (a) VIRTUAL, UNIQUE (v)) PARTITION BY RANGE (a)",
            lacks("UNIQUE", "tv", "a"),
        ),
        (
            "CREATE TABLE te (a int, b int, UNIQUE (b)) PARTITION BY RANGE ((a + 1))",
            (
                "0A000".into(),
                "unsupported UNIQUE constraint with partition key definition".into(),
                Some(
                    "UNIQUE constraints cannot be used when partition keys include expressions."
                        .into(),
                ),
            ),
        ),
        (
            "CREATE TABLE tw (id int, valid_at daterange, PRIMARY KEY (id, valid_at WITHOUT OVERLAPS)) PARTITION BY RANGE (valid_at)",
            (
                "0A000".into(),
                "cannot match partition key to index on column \"valid_at\" using non-equal operator \"&&\"".into(),
                None,
            ),
        ),
        (
            "CREATE TABLE tl (a int, b int, UNIQUE (a)) PARTITION BY LIST (a, b)",
            (
                "42P17".into(),
                "cannot use \"list\" partition strategy with more than one column".into(),
                None,
            ),
        ),
    ] {
        assert_eq!(failure(&engine, sql), expected, "{sql}");
    }
    exec(
        &engine,
        "CREATE TABLE ok1 (a int, b int, PRIMARY KEY (a, b)) PARTITION BY RANGE (a);
         CREATE TABLE ok2 (id int, valid_at daterange, PRIMARY KEY (id, valid_at WITHOUT OVERLAPS)) PARTITION BY LIST (id);
         CREATE TABLE ok3 (a int, b int, UNIQUE (b, a)) PARTITION BY HASH (a)",
    );
}

#[test]
fn a_partitioned_partition_checks_the_keys_it_inherits_before_its_own() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE tsub (a int, b int, c int, PRIMARY KEY (a, b)) PARTITION BY RANGE (a)",
    );
    assert_eq!(
        failure(
            &engine,
            "CREATE TABLE tsub_1 PARTITION OF tsub FOR VALUES FROM (0) TO (10) PARTITION BY RANGE (c)"
        ),
        lacks("PRIMARY KEY", "tsub_1", "c")
    );
    assert_eq!(
        failure(
            &engine,
            "CREATE TABLE tsub_2 PARTITION OF tsub (UNIQUE (c)) FOR VALUES FROM (10) TO (20) PARTITION BY RANGE (b)"
        ),
        lacks("UNIQUE", "tsub_2", "b")
    );
    exec(
        &engine,
        "CREATE TABLE tsub_3 PARTITION OF tsub (UNIQUE (c, b, a)) FOR VALUES FROM (20) TO (30) PARTITION BY RANGE (b)",
    );
}

#[test]
fn a_key_added_to_a_partitioned_table_checks_every_partition() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE p (a int, b int) PARTITION BY RANGE (a);
         CREATE TABLE p1 PARTITION OF p FOR VALUES FROM (0) TO (10);
         CREATE TABLE p2 PARTITION OF p FOR VALUES FROM (10) TO (20) PARTITION BY RANGE (b);
         CREATE TABLE p21 PARTITION OF p2 FOR VALUES FROM (0) TO (100);
         ALTER TABLE p ADD UNIQUE (a, b)",
    );
    assert_eq!(
        failure(&engine, "ALTER TABLE p ADD UNIQUE (b)"),
        lacks("UNIQUE", "p", "a")
    );
    assert_eq!(
        failure(&engine, "ALTER TABLE p ADD UNIQUE (a)"),
        lacks("UNIQUE", "p2", "b")
    );
    assert_eq!(
        failure(&engine, "ALTER TABLE p ADD PRIMARY KEY (a)"),
        lacks("PRIMARY KEY", "p2", "b")
    );
    // The rejected keys left nothing behind.
    exec(&engine, "INSERT INTO p VALUES (1, 1), (1, 2), (11, 1)");
    let error = engine.sql("INSERT INTO p VALUES (1, 1)", &[]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "duplicate key value violates unique constraint \"p1_a_b_key\""
    );
}

#[test]
fn a_key_added_to_a_partitioned_table_checks_each_partition_rows() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE q (a int, b int) PARTITION BY RANGE (a);
         CREATE TABLE q1 PARTITION OF q FOR VALUES FROM (0) TO (10);
         INSERT INTO q VALUES (1, 1), (1, 1), (2, NULL), (2, NULL)",
    );
    for (sql, index, detail) in [
        (
            "ALTER TABLE q ADD UNIQUE (a, b)",
            "q1_a_b_key",
            "Key (a, b)=(1, 1) is duplicated.",
        ),
        (
            "ALTER TABLE q ADD PRIMARY KEY (a)",
            "q1_pkey",
            "Key (a)=(1) is duplicated.",
        ),
    ] {
        assert_eq!(
            failure(&engine, sql),
            (
                "23505".into(),
                format!("could not create unique index \"{index}\""),
                Some(detail.into())
            ),
            "{sql}"
        );
    }
    // Partitions build their indexes in partition order, each before its own partitions.
    exec(
        &engine,
        "CREATE TABLE r (a int, b int) PARTITION BY RANGE (a);
         CREATE TABLE r1 PARTITION OF r FOR VALUES FROM (0) TO (10) PARTITION BY RANGE (b);
         CREATE TABLE r11 PARTITION OF r1 FOR VALUES FROM (0) TO (10);
         CREATE TABLE r2 PARTITION OF r FOR VALUES FROM (10) TO (20);
         INSERT INTO r VALUES (1, 1), (1, 1), (11, 1), (11, 1)",
    );
    assert_eq!(
        failure(&engine, "ALTER TABLE r ADD UNIQUE (a, b)"),
        (
            "23505".into(),
            "could not create unique index \"r11_a_b_key\"".into(),
            Some("Key (a, b)=(1, 1) is duplicated.".into())
        )
    );
    // The NOT NULL constraints of a primary key are verified on each partition after every index is built.
    exec(
        &engine,
        "CREATE TABLE n (a int, b int) PARTITION BY RANGE (a);
         CREATE TABLE n1 PARTITION OF n FOR VALUES FROM (0) TO (10);
         CREATE TABLE n2 PARTITION OF n FOR VALUES FROM (10) TO (20);
         INSERT INTO n VALUES (1, 5), (11, NULL)",
    );
    assert_eq!(
        failure(&engine, "ALTER TABLE n ADD PRIMARY KEY (a, b)"),
        (
            "23502".into(),
            "column \"b\" of relation \"n2\" contains null values".into(),
            None
        )
    );
    exec(
        &engine,
        "DELETE FROM n WHERE b IS NULL; ALTER TABLE n ADD PRIMARY KEY (a, b)",
    );
}
