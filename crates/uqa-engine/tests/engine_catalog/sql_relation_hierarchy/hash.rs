//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 hash-partition routing and validation.

use super::{exec, Engine, Value};

fn create_hash_partitions(engine: &Engine, parent: &str, remainders: &[i32]) {
    for remainder in remainders {
        exec(
            engine,
            &format!(
                "CREATE TABLE {parent}_r{remainder} PARTITION OF {parent} FOR VALUES WITH (MODULUS 17, REMAINDER {remainder})"
            ),
        );
    }
}

fn leaf_count(engine: &Engine, parent: &str, remainder: i32) -> usize {
    engine
        .sql(&format!("SELECT * FROM {parent}_r{remainder}"), &[])
        .unwrap()
        .rows
        .len()
}

#[test]
fn hash_partitions_match_postgresql_18_modulo_seventeen_and_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hash-hierarchy.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE hash_bigints (k BIGINT) PARTITION BY HASH (k)",
        );
        create_hash_partitions(&engine, "hash_bigints", &[0, 2, 7, 10, 12, 13, 14]);
        exec(
            &engine,
            "INSERT INTO hash_bigints VALUES (-2147483649), (-2147483648), (-1), (0), (1), (2), (17), (2147483647), (2147483648)",
        );
        assert_eq!(leaf_count(&engine, "hash_bigints", 0), 2);
        assert_eq!(leaf_count(&engine, "hash_bigints", 2), 2);
        assert_eq!(leaf_count(&engine, "hash_bigints", 7), 1);
        assert_eq!(leaf_count(&engine, "hash_bigints", 10), 2);
        assert_eq!(leaf_count(&engine, "hash_bigints", 12), 1);
        assert_eq!(leaf_count(&engine, "hash_bigints", 13), 1);

        let wrong_child = engine
            .sql("INSERT INTO hash_bigints_r2 VALUES (1)", &[])
            .unwrap_err();
        assert_eq!(wrong_child.sqlstate(), Some("23514"));
        let moved = engine
            .sql(
                "UPDATE hash_bigints SET k = 42 WHERE k = 1 RETURNING old.k AS old_k, new.k AS new_k",
                &[],
            )
            .unwrap();
        assert_eq!(moved.rows.len(), 1);
        assert_eq!(moved.rows[0]["old_k"], Value::Int(1));
        assert_eq!(moved.rows[0]["new_k"], Value::Int(42));
        assert_eq!(leaf_count(&engine, "hash_bigints", 7), 0);
        assert_eq!(leaf_count(&engine, "hash_bigints", 14), 1);

        exec(
            &engine,
            "CREATE TABLE hash_texts (k TEXT) PARTITION BY HASH (k)",
        );
        create_hash_partitions(&engine, "hash_texts", &[0, 1, 4, 5, 9, 15]);
        exec(
            &engine,
            "INSERT INTO hash_texts VALUES (NULL), (''), ('a'), ('alpha'), ('한글'), ('é'), ('cognica')",
        );
        assert_eq!(leaf_count(&engine, "hash_texts", 0), 1);
        assert_eq!(leaf_count(&engine, "hash_texts", 1), 1);
        assert_eq!(leaf_count(&engine, "hash_texts", 4), 2);
        assert_eq!(leaf_count(&engine, "hash_texts", 5), 1);
        assert_eq!(leaf_count(&engine, "hash_texts", 9), 1);
        assert_eq!(leaf_count(&engine, "hash_texts", 15), 1);

        exec(
            &engine,
            "CREATE TABLE hash_uuids (k UUID) PARTITION BY HASH (k)",
        );
        create_hash_partitions(&engine, "hash_uuids", &[10, 11, 12]);
        exec(
            &engine,
            "INSERT INTO hash_uuids VALUES ('00000000-0000-0000-0000-000000000000'), ('550e8400-e29b-41d4-a716-446655440000'), ('ffffffff-ffff-ffff-ffff-ffffffffffff')",
        );
        assert_eq!(leaf_count(&engine, "hash_uuids", 10), 1);
        assert_eq!(leaf_count(&engine, "hash_uuids", 11), 1);
        assert_eq!(leaf_count(&engine, "hash_uuids", 12), 1);

        exec(
            &engine,
            "CREATE TABLE hash_dates (k DATE) PARTITION BY HASH (k)",
        );
        create_hash_partitions(&engine, "hash_dates", &[5, 7, 10, 13]);
        exec(
            &engine,
            "INSERT INTO hash_dates VALUES ('1970-01-01'), ('1999-12-31'), ('2000-01-01'), ('2026-08-25')",
        );
        assert_eq!(leaf_count(&engine, "hash_dates", 5), 1);
        assert_eq!(leaf_count(&engine, "hash_dates", 7), 1);
        assert_eq!(leaf_count(&engine, "hash_dates", 10), 1);
        assert_eq!(leaf_count(&engine, "hash_dates", 13), 1);

        exec(
            &engine,
            "CREATE TABLE hash_composite (a INTEGER, b TEXT, c UUID) PARTITION BY HASH (a, b, c)",
        );
        create_hash_partitions(&engine, "hash_composite", &[0, 4, 7, 11, 14]);
        exec(
            &engine,
            "INSERT INTO hash_composite VALUES (NULL, NULL, NULL), (1, NULL, NULL), (NULL, 'alpha', NULL), (NULL, NULL, '550e8400-e29b-41d4-a716-446655440000'), (1, 'alpha', '550e8400-e29b-41d4-a716-446655440000'), (-1, '한글', 'ffffffff-ffff-ffff-ffff-ffffffffffff')",
        );
        assert_eq!(leaf_count(&engine, "hash_composite", 0), 1);
        assert_eq!(leaf_count(&engine, "hash_composite", 4), 1);
        assert_eq!(leaf_count(&engine, "hash_composite", 7), 1);
        assert_eq!(leaf_count(&engine, "hash_composite", 11), 2);
        assert_eq!(leaf_count(&engine, "hash_composite", 14), 1);
    }

    let reopened = Engine::open(&path).unwrap();
    assert_eq!(leaf_count(&reopened, "hash_bigints", 14), 1);
    assert_eq!(leaf_count(&reopened, "hash_texts", 9), 1);
    assert_eq!(leaf_count(&reopened, "hash_uuids", 11), 1);
    assert_eq!(leaf_count(&reopened, "hash_dates", 5), 1);
    assert_eq!(leaf_count(&reopened, "hash_composite", 0), 1);
    exec(&reopened, "INSERT INTO hash_bigints VALUES (1)");
    assert_eq!(leaf_count(&reopened, "hash_bigints", 7), 1);
}

#[test]
fn hash_partition_ddl_rejects_invalid_bounds_defaults_and_modulus_chains() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE hash_validation (k INTEGER) PARTITION BY HASH (k)",
    );
    for (sql, message) in [
        (
            "CREATE TABLE hash_zero PARTITION OF hash_validation FOR VALUES WITH (MODULUS 0, REMAINDER 0)",
            "modulus for hash partition must be an integer value greater than zero",
        ),
        (
            "CREATE TABLE hash_equal PARTITION OF hash_validation FOR VALUES WITH (MODULUS 4, REMAINDER 4)",
            "remainder for hash partition must be less than modulus",
        ),
        (
            "CREATE TABLE hash_default PARTITION OF hash_validation DEFAULT",
            "a hash-partitioned table may not have a default partition",
        ),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P16"));
        assert_eq!(error.to_string(), message);
    }
    assert!(!engine.has_table("hash_zero").unwrap());
    assert!(!engine.has_table("hash_equal").unwrap());
    assert!(!engine.has_table("hash_default").unwrap());

    exec(
        &engine,
        "CREATE TABLE hash_mod4_r0 PARTITION OF hash_validation FOR VALUES WITH (MODULUS 4, REMAINDER 0)",
    );
    exec(
        &engine,
        "CREATE TABLE hash_mod8_r1 PARTITION OF hash_validation FOR VALUES WITH (MODULUS 8, REMAINDER 1)",
    );
    let invalid_chain = engine
        .sql(
            "CREATE TABLE hash_mod6_r0 PARTITION OF hash_validation FOR VALUES WITH (MODULUS 6, REMAINDER 0)",
            &[],
        )
        .unwrap_err();
    assert_eq!(invalid_chain.sqlstate(), Some("42P17"));
    assert!(invalid_chain
        .to_string()
        .contains("every hash partition modulus must be a factor"));
    assert!(!engine.has_table("hash_mod6_r0").unwrap());

    let overlap = engine
        .sql(
            "CREATE TABLE hash_mod8_r4 PARTITION OF hash_validation FOR VALUES WITH (MODULUS 8, REMAINDER 4)",
            &[],
        )
        .unwrap_err();
    assert_eq!(overlap.sqlstate(), Some("42P17"));
    assert!(overlap.to_string().contains("would overlap partition"));
    assert!(!engine.has_table("hash_mod8_r4").unwrap());

    let expression = engine
        .sql(
            "CREATE TABLE hash_expression (k INTEGER) PARTITION BY HASH ((k + 1))",
            &[],
        )
        .unwrap_err();
    assert_eq!(expression.sqlstate(), Some("0A000"));
    assert!(expression
        .to_string()
        .contains("HASH partition key expressions"));
    assert!(!engine.has_table("hash_expression").unwrap());
}
