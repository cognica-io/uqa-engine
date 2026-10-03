//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PRIMARY KEY and UNIQUE declarations of CREATE TABLE and ALTER TABLE, checked in the order of `PostgreSQL`'s `transformIndexConstraint` and `DefineIndex` with their errors, and the keys a CREATE TABLE keeps.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn error(engine: &Engine, sql: &str, state: &str, message: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    assert_eq!(error.to_string(), message, "{sql}");
}

/// The detail of the error `sql` fails with.
fn detail(engine: &Engine, sql: &str) -> Option<String> {
    match engine.sql(sql, &[]).unwrap_err() {
        uqa_sql::SQLError::Diagnostic { detail, .. } => detail,
        other => panic!("{sql}: {other} carries no detail"),
    }
}

/// The name and type of each constraint of `table`, by name.
fn constraints(engine: &Engine, table: &str) -> Vec<(String, String)> {
    let result = engine
        .sql(
            &format!(
                "SELECT conname::text AS name, contype::text AS kind FROM pg_constraint WHERE conrelid = '{table}'::regclass ORDER BY conname"
            ),
            &[],
        )
        .unwrap();
    result
        .rows
        .iter()
        .map(|row| match (&row["name"], &row["kind"]) {
            (Value::Str(name), Value::Str(kind)) => (name.clone(), kind.clone()),
            other => panic!("unexpected constraint row {other:?}"),
        })
        .collect()
}

fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(name, kind)| ((*name).to_string(), (*kind).to_string()))
        .collect()
}

#[test]
fn create_table_key_declarations_fail_as_postgresql_analyzes_them() {
    let engine = Engine::new();
    for (sql, state, message) in [
        (
            "CREATE TABLE e1 (a int PRIMARY KEY, b int PRIMARY KEY)",
            "42P16",
            "multiple primary keys for table \"e1\" are not allowed",
        ),
        (
            "CREATE TABLE e2 (a int, PRIMARY KEY (x))",
            "42703",
            "column \"x\" named in key does not exist",
        ),
        (
            "CREATE TABLE e3 (a int, UNIQUE (a, a))",
            "42701",
            "column \"a\" appears twice in unique constraint",
        ),
        (
            "CREATE TABLE e4 (a int, PRIMARY KEY (a, a))",
            "42701",
            "column \"a\" appears twice in primary key constraint",
        ),
        (
            "CREATE TABLE e5 (a int, UNIQUE (a) INCLUDE (zz))",
            "42703",
            "column \"zz\" named in key does not exist",
        ),
        // A second primary key fails before its own columns are resolved, and after the columns of the keys before it.
        (
            "CREATE TABLE m1 (a int, PRIMARY KEY (a), PRIMARY KEY (x))",
            "42P16",
            "multiple primary keys for table \"m1\" are not allowed",
        ),
        (
            "CREATE TABLE m2 (a int, PRIMARY KEY (x), PRIMARY KEY (a))",
            "42703",
            "column \"x\" named in key does not exist",
        ),
        // The period of a WITHOUT OVERLAPS key is checked with the key columns, the key's length after them and the included columns last.
        (
            "CREATE TABLE w1 (a daterange, UNIQUE (x WITHOUT OVERLAPS))",
            "42703",
            "column \"x\" named in key does not exist",
        ),
        (
            "CREATE TABLE w2 (a int, UNIQUE (a WITHOUT OVERLAPS))",
            "42804",
            "column \"a\" in WITHOUT OVERLAPS is not a range or multirange type",
        ),
        (
            "CREATE TABLE w3 (a daterange, UNIQUE (a WITHOUT OVERLAPS) INCLUDE (zz))",
            "42601",
            "constraint using WITHOUT OVERLAPS needs at least two columns",
        ),
        (
            "CREATE TABLE w4 (a int, UNIQUE (a, ctid WITHOUT OVERLAPS))",
            "42804",
            "column \"ctid\" in WITHOUT OVERLAPS is not a range or multirange type",
        ),
    ] {
        error(&engine, sql, state, message);
    }
}

#[test]
fn create_table_keys_fail_on_their_index_attributes_and_names() {
    let engine = Engine::new();
    for (sql, state, message) in [
        // A system or virtual generated column passes the declaration and fails as an index attribute.
        (
            "CREATE TABLE s1 (a int, UNIQUE (ctid))",
            "0A000",
            "index creation on system columns is not supported",
        ),
        (
            "CREATE TABLE s2 (a int, PRIMARY KEY (a) INCLUDE (ctid))",
            "0A000",
            "index creation on system columns is not supported",
        ),
        (
            "CREATE TABLE v1 (a int, v int GENERATED ALWAYS AS (a) VIRTUAL, UNIQUE (a) INCLUDE (v))",
            "0A000",
            "unique constraints on virtual generated columns are not supported",
        ),
        (
            "CREATE TABLE v2 (a int, v int GENERATED ALWAYS AS (a) VIRTUAL, PRIMARY KEY (v))",
            "0A000",
            "primary keys on virtual generated columns are not supported",
        ),
        (
            "CREATE TABLE v3 (a int, v int GENERATED ALWAYS AS (a) VIRTUAL, UNIQUE (ctid, v))",
            "0A000",
            "index creation on system columns is not supported",
        ),
        // A key's name is its index's name: a repeated key name names a relation that exists, and the name of another constraint a constraint that exists.
        (
            "CREATE TABLE n1 (a int, b int, CONSTRAINT c UNIQUE (a), CONSTRAINT c UNIQUE (b))",
            "42P07",
            "relation \"c\" already exists",
        ),
        (
            "CREATE TABLE n2 (a int, CONSTRAINT c CHECK (a > 0), CONSTRAINT c UNIQUE (a))",
            "42710",
            "constraint \"c\" for relation \"n2\" already exists",
        ),
        (
            "CREATE TABLE n3 (a int CONSTRAINT c NOT NULL, CONSTRAINT c UNIQUE (a))",
            "42710",
            "constraint \"c\" for relation \"n3\" already exists",
        ),
        (
            "CREATE TABLE n4 (a int, CONSTRAINT c UNIQUE (a), CONSTRAINT c FOREIGN KEY (a) REFERENCES n4 (a))",
            "42710",
            "constraint \"c\" for relation \"n4\" already exists",
        ),
    ] {
        error(&engine, sql, state, message);
    }
    // The declared keys are analyzed before the relation's name is checked; IF NOT EXISTS skips first.
    exec(&engine, "CREATE TABLE d (a int)");
    error(
        &engine,
        "CREATE TABLE d (a int, PRIMARY KEY (x))",
        "42703",
        "column \"x\" named in key does not exist",
    );
    exec(
        &engine,
        "CREATE TABLE IF NOT EXISTS d (a int, PRIMARY KEY (x))",
    );
}

#[test]
fn create_table_builds_the_primary_key_first_and_no_repeated_index() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE d1 (a int UNIQUE PRIMARY KEY);
         CREATE TABLE d2 (a int UNIQUE, b int, UNIQUE (a));
         CREATE TABLE d3 (a int, UNIQUE (a), CONSTRAINT named_u UNIQUE (a));
         CREATE TABLE d4 (a int, CONSTRAINT u1 UNIQUE (a), CONSTRAINT u2 UNIQUE (a));
         CREATE TABLE d5 (a int CONSTRAINT pk_name UNIQUE, PRIMARY KEY (a));
         CREATE TABLE d6 (a int, b int, UNIQUE (a), CONSTRAINT u3 UNIQUE (a), UNIQUE (b) INCLUDE (a), UNIQUE (b) INCLUDE (a));
         CREATE TABLE d7 (a int, b daterange, UNIQUE (a, b WITHOUT OVERLAPS), UNIQUE (a, b));
         CREATE TABLE d8 (a int, UNIQUE NULLS NOT DISTINCT (a), UNIQUE (a));
         CREATE TABLE d9 (a int, CONSTRAINT d9_pkey UNIQUE (a), PRIMARY KEY (a))",
    );
    for (table, expected) in [
        ("d1", &[("d1_a_not_null", "n"), ("d1_pkey", "p")][..]),
        ("d2", &[("d2_a_key", "u")]),
        ("d3", &[("named_u", "u")]),
        ("d4", &[("u1", "u")]),
        ("d5", &[("d5_a_not_null", "n"), ("pk_name", "p")]),
        ("d6", &[("d6_b_a_key", "u"), ("u3", "u")]),
        ("d7", &[("d7_a_b_key", "u"), ("d7_a_b_key1", "u")]),
        ("d8", &[("d8_a_key", "u"), ("d8_a_key1", "u")]),
        ("d9", &[("d9_a_not_null", "n"), ("d9_pkey", "p")]),
    ] {
        assert_eq!(constraints(&engine, table), pairs(expected), "{table}");
    }
    let indexes = engine
        .sql(
            "SELECT count(*) AS n FROM pg_index WHERE indrelid = 'd1'::regclass",
            &[],
        )
        .unwrap();
    assert_eq!(indexes.rows[0]["n"], Value::Int(1));
    exec(&engine, "INSERT INTO d2 VALUES (1, 1)");
    error(
        &engine,
        "INSERT INTO d2 VALUES (1, 2)",
        "23505",
        "duplicate key value violates unique constraint \"d2_a_key\"",
    );
}

#[test]
fn repeated_keys_stay_dropped_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keys.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE d1 (a int UNIQUE PRIMARY KEY); CREATE TABLE d2 (a int UNIQUE, UNIQUE (a))",
        );
    }
    let engine = Engine::open(&path).unwrap();
    assert_eq!(
        constraints(&engine, "d1"),
        pairs(&[("d1_a_not_null", "n"), ("d1_pkey", "p")])
    );
    assert_eq!(constraints(&engine, "d2"), pairs(&[("d2_a_key", "u")]));
}

#[test]
fn a_declared_primary_key_names_inherited_columns_and_makes_their_not_null_local() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE p (a int NOT NULL, z int);
         CREATE TABLE c1 (a int, PRIMARY KEY (a)) INHERITS (p);
         CREATE TABLE c2 (b int, PRIMARY KEY (a)) INHERITS (p);
         CREATE TABLE c3 (b int, PRIMARY KEY (z)) INHERITS (p);
         CREATE TABLE pp (a int NOT NULL, z int) PARTITION BY RANGE (a);
         CREATE TABLE pp1 PARTITION OF pp (PRIMARY KEY (a, z)) FOR VALUES FROM (0) TO (10)",
    );
    let rows = engine
        .sql(
            "SELECT conrelid::regclass::text AS rel, conname::text AS name, conislocal, coninhcount FROM pg_constraint WHERE conrelid IN ('c1'::regclass, 'c2'::regclass, 'c3'::regclass, 'pp1'::regclass) ORDER BY conname",
            &[],
        )
        .unwrap();
    let rows = rows
        .rows
        .iter()
        .map(|row| {
            (
                row["name"].clone(),
                row["conislocal"].clone(),
                row["coninhcount"].clone(),
            )
        })
        .collect::<Vec<_>>();
    let expected = [
        ("c1_a_not_null", true, 1),
        ("c1_pkey", true, 0),
        ("c2_a_not_null", true, 1),
        ("c2_pkey", true, 0),
        ("c3_pkey", true, 0),
        ("c3_z_not_null", true, 0),
        ("p_a_not_null", false, 1),
        ("pp1_a_not_null", true, 1),
        ("pp1_pkey", true, 0),
        ("pp1_z_not_null", true, 0),
    ]
    .map(|(name, local, inherited)| {
        (
            Value::Str(name.into()),
            Value::Bool(local),
            Value::Int(inherited),
        )
    });
    assert_eq!(rows, expected);
    error(
        &engine,
        "CREATE TABLE c4 (b int, PRIMARY KEY (zz)) INHERITS (p)",
        "42703",
        "column \"zz\" named in key does not exist",
    );
    error(
        &engine,
        "CREATE TABLE c5 (b int, PRIMARY KEY (a)) INHERITS (missing)",
        "42P01",
        "relation \"missing\" does not exist",
    );
}

#[test]
fn a_partition_declares_keys_beside_the_keys_it_inherits() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE kp (a int PRIMARY KEY) PARTITION BY RANGE (a);
         CREATE TABLE ku (a int UNIQUE) PARTITION BY RANGE (a);
         CREATE TABLE ku1 PARTITION OF ku (UNIQUE (a)) FOR VALUES FROM (0) TO (10);
         CREATE TABLE ku2 PARTITION OF ku (UNIQUE (a), UNIQUE (a)) FOR VALUES FROM (10) TO (20);
         CREATE TABLE kp2 PARTITION OF kp (CONSTRAINT kp2_own UNIQUE (a)) FOR VALUES FROM (10) TO (20)",
    );
    error(
        &engine,
        "CREATE TABLE kp1 PARTITION OF kp (PRIMARY KEY (a)) FOR VALUES FROM (0) TO (10)",
        "42P16",
        "multiple primary keys for table \"kp1\" are not allowed",
    );
    assert_eq!(
        constraints(&engine, "ku1"),
        pairs(&[("ku1_a_key", "u"), ("ku1_a_key1", "u")])
    );
    assert_eq!(
        constraints(&engine, "ku2"),
        pairs(&[("ku2_a_key", "u"), ("ku2_a_key1", "u")])
    );
    assert_eq!(
        constraints(&engine, "kp2"),
        pairs(&[("kp2_own", "u"), ("kp2_pkey", "p"), ("kp_a_not_null", "n")])
    );
}

#[test]
fn alter_table_add_key_fails_as_postgresql_adds_the_key() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE k (a int, b int, c int, CONSTRAINT k_chk CHECK (a > 0));
         CREATE TABLE v (a int, g int GENERATED ALWAYS AS (a) VIRTUAL)",
    );
    for (sql, state, message) in [
        // A primary key first adds NOT NULL constraints to its columns.
        (
            "ALTER TABLE k ADD PRIMARY KEY (x)",
            "42703",
            "column \"x\" of relation \"k\" does not exist",
        ),
        (
            "ALTER TABLE k ADD PRIMARY KEY (a, x)",
            "42703",
            "column \"x\" of relation \"k\" does not exist",
        ),
        (
            "ALTER TABLE k ADD PRIMARY KEY (ctid)",
            "0A000",
            "cannot add not-null constraint on system column \"ctid\"",
        ),
        (
            "ALTER TABLE k ADD UNIQUE (x)",
            "42703",
            "column \"x\" named in key does not exist",
        ),
        (
            "ALTER TABLE k ADD PRIMARY KEY (a) INCLUDE (zz)",
            "42703",
            "column \"zz\" named in key does not exist",
        ),
        (
            "ALTER TABLE k ADD PRIMARY KEY (a, a)",
            "42701",
            "column \"a\" appears twice in primary key constraint",
        ),
        (
            "ALTER TABLE k ADD UNIQUE (a, a)",
            "42701",
            "column \"a\" appears twice in unique constraint",
        ),
        (
            "ALTER TABLE k ADD UNIQUE (ctid)",
            "0A000",
            "index creation on system columns is not supported",
        ),
        (
            "ALTER TABLE v ADD UNIQUE (g)",
            "0A000",
            "unique constraints on virtual generated columns are not supported",
        ),
        (
            "ALTER TABLE v ADD PRIMARY KEY (g)",
            "0A000",
            "primary keys on virtual generated columns are not supported",
        ),
        (
            "ALTER TABLE k ADD CONSTRAINT k_chk UNIQUE (a)",
            "42710",
            "constraint \"k_chk\" for relation \"k\" already exists",
        ),
        (
            "ALTER TABLE k ADD CONSTRAINT k UNIQUE (a)",
            "42P07",
            "relation \"k\" already exists",
        ),
    ] {
        error(&engine, sql, state, message);
    }
}

#[test]
fn alter_table_add_key_checks_the_rows_and_the_existing_keys() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE k (a int, b int, c int); INSERT INTO k VALUES (1, NULL, 1), (1, NULL, 2)",
    );
    // The index is built before the NOT NULL constraints are verified.
    error(
        &engine,
        "ALTER TABLE k ADD PRIMARY KEY (b)",
        "23502",
        "column \"b\" of relation \"k\" contains null values",
    );
    error(
        &engine,
        "ALTER TABLE k ADD PRIMARY KEY (a)",
        "23505",
        "could not create unique index \"k_pkey\"",
    );
    assert_eq!(
        detail(&engine, "ALTER TABLE k ADD UNIQUE (a)").as_deref(),
        Some("Key (a)=(1) is duplicated.")
    );
    exec(&engine, "ALTER TABLE k ADD PRIMARY KEY (c)");
    for (sql, state, message) in [
        (
            "ALTER TABLE k ADD PRIMARY KEY (b, x)",
            "42703",
            "column \"x\" of relation \"k\" does not exist",
        ),
        (
            "ALTER TABLE k ADD PRIMARY KEY (c)",
            "42P16",
            "multiple primary keys for table \"k\" are not allowed",
        ),
        (
            "ALTER TABLE k ADD CONSTRAINT k_pkey UNIQUE (b)",
            "42P07",
            "relation \"k_pkey\" already exists",
        ),
    ] {
        error(&engine, sql, state, message);
    }
}

#[test]
fn alter_table_add_column_keys_fail_after_the_column() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (a int); INSERT INTO t VALUES (1), (2)",
    );
    error(
        &engine,
        "ALTER TABLE t ADD COLUMN v int GENERATED ALWAYS AS (a) VIRTUAL UNIQUE",
        "0A000",
        "unique constraints on virtual generated columns are not supported",
    );
    error(
        &engine,
        "ALTER TABLE t ADD COLUMN p int PRIMARY KEY",
        "23502",
        "column \"p\" of relation \"t\" contains null values",
    );
    error(
        &engine,
        "ALTER TABLE t ADD COLUMN q int DEFAULT 7 PRIMARY KEY",
        "23505",
        "could not create unique index \"t_pkey\"",
    );
    // An inheritance child receives the column and the primary key's NOT NULL constraint, never the keys' indexes.
    exec(
        &engine,
        "CREATE TABLE ip (a int); CREATE TABLE ic () INHERITS (ip);
         ALTER TABLE ip ADD COLUMN c int UNIQUE; ALTER TABLE ip ADD COLUMN d int PRIMARY KEY",
    );
    assert_eq!(
        constraints(&engine, "ip"),
        pairs(&[("ip_c_key", "u"), ("ip_d_not_null", "n"), ("ip_pkey", "p")])
    );
    assert_eq!(constraints(&engine, "ic"), pairs(&[("ip_d_not_null", "n")]));
    exec(&engine, "INSERT INTO ic VALUES (1, 1, 1), (1, 1, 1)");
}
