//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric operator identity in public SQL, prepared statements and durable definitions.

use super::*;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

fn verify_function_search_path(engine: &Engine) {
    for (sql, builtin) in [
        ("mod(7,3)", Value::Int(1)),
        ("power(2::float8,3::float8)", Value::Float(8.0)),
        ("pow(2::float8,3::float8)", Value::Float(8.0)),
        ("sqrt(9::float8)", Value::Float(3.0)),
        ("cbrt(8::float8)", Value::Float(2.0)),
        ("abs(-1)", Value::Int(1)),
    ] {
        let shadow = if matches!(builtin, Value::Int(_)) {
            Value::Int(99)
        } else {
            Value::Float(99.0)
        };
        assert_eq!(
            engine
                .sql(&format!("SELECT {sql} AS value"), &[])
                .unwrap()
                .rows[0]["value"],
            shadow,
            "{sql}"
        );
        assert_eq!(
            engine
                .sql(&format!("SELECT pg_catalog.{sql} AS value"), &[])
                .unwrap()
                .rows[0]["value"],
            builtin,
            "{sql}"
        );
        engine
            .sql("SET search_path = pg_catalog, shadow, public", &[])
            .unwrap();
        assert_eq!(
            engine
                .sql(&format!("SELECT {sql} AS value"), &[])
                .unwrap()
                .rows[0]["value"],
            builtin,
            "{sql}"
        );
        engine
            .sql("SET search_path = shadow, pg_catalog, public", &[])
            .unwrap();
    }
}

fn verify(engine: &Engine) {
    engine
        .sql("SET search_path = shadow, pg_catalog, public", &[])
        .unwrap();
    let result = engine
        .sql(
            "SELECT remainder, absolute, raised, root FROM numbers ORDER BY source",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["remainder"], Value::Int(0));
    assert_eq!(result.rows[0]["absolute"], Value::Int(4));
    assert_eq!(result.rows[0]["raised"], Value::Float(16.0));
    assert_eq!(result.rows[0]["root"], Value::Float(2.0));
    let calls = engine
        .sql(
            "SELECT 7 % 3 AS op, mod(7,3) AS routine, (2 ^ 3) AS raised",
            &[],
        )
        .unwrap();
    assert_eq!(calls.rows[0]["op"], Value::Int(1));
    assert_eq!(calls.rows[0]["routine"], Value::Int(99));
    assert_eq!(calls.rows[0]["raised"], Value::Float(8.0));
    verify_function_search_path(engine);
    assert_eq!(
        engine.sql("SELECT 7 % 3", &[]).unwrap().rows[0]["?column?"],
        Value::Int(1)
    );
    for sql in [
        "SELECT @ (SELECT (-32768)::smallint)",
        "SELECT @ low_smallint()",
        "SELECT pg_catalog.abs((-32768)::smallint)",
        "SELECT pg_catalog.abs((-2147483648)::integer)",
    ] {
        assert_eq!(
            engine.sql(sql, &[]).unwrap_err().sqlstate(),
            Some("22003"),
            "{sql}"
        );
    }
    assert_eq!(
        engine.sql("SELECT 3::even", &[]).unwrap_err().sqlstate(),
        Some("23514")
    );
    engine.sql("SELECT 4::even", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT (-32768)::small_magnitude", &[])
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    assert_eq!(
        engine
            .sql("INSERT INTO numbers(source) VALUES(-32768)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    engine.sql("PREPARE numeric_inputs AS SELECT $1 % 2 AS r, |/ $2 AS root; EXECUTE numeric_inputs(7,9)", &[]).unwrap();
    let values = engine.sql("EXECUTE numeric_inputs(7,9)", &[]).unwrap();
    assert_eq!(values.rows[0]["r"], Value::Int(1));
    assert_eq!(values.rows[0]["root"], Value::Float(3.0));
    engine.sql("DEALLOCATE numeric_inputs", &[]).unwrap();
    let expected = engine.sql("SELECT * FROM number_view", &[]).unwrap();
    let definition = engine
        .sql("SELECT pg_get_viewdef('number_view'::regclass) AS sql", &[])
        .unwrap();
    let Value::Str(sql) = &definition.rows[0]["sql"] else {
        panic!("view definition")
    };
    engine
        .sql(&format!("CREATE VIEW number_view_copy AS {sql}"), &[])
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT * FROM number_view_copy", &[])
            .unwrap()
            .rows,
        expected.rows
    );
    engine.sql("DROP VIEW number_view_copy", &[]).unwrap();
}

#[test]
fn numeric_operator_definitions_survive_function_shadowing_and_provider_reopen() {
    for provider in 0..3 {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("numeric.db");
        let engine = open(provider, &path);
        engine.sql("CREATE SCHEMA shadow; SET search_path = shadow, pg_catalog, public;
            CREATE FUNCTION shadow.mod(integer,integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS 'SELECT 99';
            CREATE FUNCTION shadow.power(double precision,double precision) RETURNS double precision LANGUAGE SQL IMMUTABLE AS 'SELECT 99::float8';
            CREATE FUNCTION shadow.pow(double precision,double precision) RETURNS double precision LANGUAGE SQL IMMUTABLE AS 'SELECT 99::float8';
            CREATE FUNCTION shadow.sqrt(double precision) RETURNS double precision LANGUAGE SQL IMMUTABLE AS 'SELECT 99::float8';
            CREATE FUNCTION shadow.cbrt(double precision) RETURNS double precision LANGUAGE SQL IMMUTABLE AS 'SELECT 99::float8';
            CREATE FUNCTION shadow.abs(integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS 'SELECT 99';
            CREATE FUNCTION shadow.\"^\"(integer,integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS 'SELECT 99';
            CREATE FUNCTION shadow.low_smallint() RETURNS smallint LANGUAGE SQL IMMUTABLE AS 'SELECT (-32768)::smallint';
            CREATE DOMAIN even AS integer CHECK(VALUE % 2 = 0);
            CREATE DOMAIN small_magnitude AS smallint CHECK(@ VALUE > 0);
            CREATE TABLE numbers(source smallint,
                remainder smallint GENERATED ALWAYS AS (source % 2::smallint) STORED,
                absolute smallint GENERATED ALWAYS AS (@ source) STORED,
                raised double precision GENERATED ALWAYS AS (source ^ 2),
                root double precision GENERATED ALWAYS AS (|/ (@ source)));
            INSERT INTO numbers(source) VALUES(-4);
            CREATE VIEW number_view AS SELECT source % 3::smallint AS remainder, @ source AS magnitude, (source::numeric ^ 2) % 3::numeric AS residue FROM numbers",
            &[]).unwrap();
        verify(&engine);
        drop(engine);
        verify(&open(provider, &path));
    }
}
