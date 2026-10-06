//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL type-system coverage.

use uqa_core::{ArrayValue, DecimalValue, Value};
use uqa_engine::{Engine, SQLResult};

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn engine_with_table() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER, name TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (1, 10, 'alpha')",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (2, 20, 'bravo')",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (3, 30, 'charlie')",
    );
    engine
}

fn dec(value: &str) -> Value {
    Value::Decimal(DecimalValue::parse(value).unwrap())
}

fn array(values: Vec<Value>) -> Value {
    Value::Array(ArrayValue::try_new(values).unwrap())
}

#[test]
fn numeric_literal_arithmetic_is_decimal() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT 0.1 + 0.2 AS v");
    assert_eq!(result.rows[0]["v"], dec("0.3"));
}

#[test]
fn numeric_column_rounds_to_declared_scale() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE prices (id INTEGER PRIMARY KEY, amount NUMERIC(10, 2))",
    );
    exec(
        &engine,
        "INSERT INTO prices (id, amount) VALUES (1, 12.345)",
    );
    let result = exec(&engine, "SELECT amount FROM prices WHERE id = 1");
    assert_eq!(result.rows[0]["amount"], dec("12.35"));
}

#[test]
fn numeric_negative_scale_rounds_left_of_decimal() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE buckets (id INTEGER PRIMARY KEY, amount NUMERIC(2, -3))",
    );
    exec(
        &engine,
        "INSERT INTO buckets (id, amount) VALUES (1, 12345)",
    );
    let result = exec(&engine, "SELECT amount FROM buckets WHERE id = 1");
    assert_eq!(result.rows[0]["amount"], dec("12000"));
}

#[test]
fn numeric_negative_scale_enforces_precision_after_rounding() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE buckets (id INTEGER PRIMARY KEY, amount NUMERIC(2, -3))",
    );
    let err = engine
        .sql("INSERT INTO buckets (id, amount) VALUES (1, 99999)", &[])
        .unwrap_err();
    assert!(err.to_string().contains("numeric field overflow"));
}

#[test]
fn numeric_scale_larger_than_precision_restricts_fractional_range() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE tiny (id INTEGER PRIMARY KEY, amount NUMERIC(3, 5))",
    );
    exec(
        &engine,
        "INSERT INTO tiny (id, amount) VALUES (1, 0.009994)",
    );
    let result = exec(&engine, "SELECT amount FROM tiny WHERE id = 1");
    assert_eq!(result.rows[0]["amount"], dec("0.00999"));

    let err = engine
        .sql("INSERT INTO tiny (id, amount) VALUES (2, 0.009995)", &[])
        .unwrap_err();
    assert!(err.to_string().contains("numeric field overflow"));
}

#[test]
fn numeric_information_schema_reports_negative_scale() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE buckets (id INTEGER PRIMARY KEY, amount NUMERIC(2, -3))",
    );
    let result = exec(
        &engine,
        "SELECT numeric_precision, numeric_scale
         FROM information_schema.columns
         WHERE table_name = 'buckets' AND column_name = 'amount'",
    );
    assert_eq!(result.rows[0]["numeric_precision"], Value::Int(2));
    assert_eq!(result.rows[0]["numeric_scale"], Value::Int(-3));
}

#[test]
fn numeric_cast_preserves_decimal_value() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT CAST('123456789012345.6789' AS NUMERIC) AS v",
    );
    assert_eq!(result.rows[0]["v"], dec("123456789012345.6789"));
}

#[test]
fn select_array_literal() {
    let engine = engine_with_table();
    let result = exec(&engine, "SELECT ARRAY[1, 2, 3] AS v FROM t WHERE id = 1");
    assert_eq!(
        result.rows[0]["v"],
        array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
}

#[test]
fn select_text_array_literal() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT ARRAY['a', 'b', 'c'] AS v FROM t WHERE id = 1",
    );
    assert_eq!(
        result.rows[0]["v"],
        array(vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
            Value::Str("c".into()),
        ])
    );
}

#[test]
fn select_empty_array_literal() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT ARRAY[]::integer[] AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], array(Vec::new()));
}

#[test]
fn text_array_column_create_insert_round_trip() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE arr_test (id SERIAL PRIMARY KEY, tags TEXT[])",
    );
    exec(
        &engine,
        "INSERT INTO arr_test (tags) VALUES (ARRAY['python', 'sql'])",
    );
    let result = exec(&engine, "SELECT tags FROM arr_test WHERE id = 1");
    assert_eq!(
        result.rows[0]["tags"],
        array(vec![Value::Str("python".into()), Value::Str("sql".into())])
    );
}

#[test]
fn integer_array_column_create_insert_round_trip() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE int_arr (id SERIAL PRIMARY KEY, nums INTEGER[])",
    );
    exec(
        &engine,
        "INSERT INTO int_arr (nums) VALUES (ARRAY[10, 20, 30])",
    );
    let result = exec(&engine, "SELECT nums FROM int_arr WHERE id = 1");
    assert_eq!(
        result.rows[0]["nums"],
        array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
    );
}

#[test]
fn array_types_coerce_elements_and_survive_engine_reopen() {
    use uqa_sql::ast::ColumnType;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("arrays.sqlite");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE array_values (
                 id INTEGER PRIMARY KEY,
                 tags TEXT[],
                 nums INTEGER[],
                 matrix INTEGER[][]
             )",
        );
        exec(
            &engine,
            "INSERT INTO array_values VALUES
                 (1, ARRAY[1, 2], ARRAY['10', '20']::INTEGER[],
                  ARRAY[ARRAY['1', '2'], ARRAY['3', '4']]::INTEGER[][]),
                 (2, '{alpha,beta}', '{30,40}', '{{5,6},{7,8}}')",
        );
        let error = engine
            .sql(
                "INSERT INTO array_values VALUES (3, ARRAY['ok'], ARRAY['bad'], ARRAY[ARRAY[1]])",
                &[],
            )
            .unwrap_err();
        assert!(error.to_string().contains("integer"));
    }

    let engine = Engine::open(&path).unwrap();
    let columns = engine.describe_table("array_values").unwrap().unwrap();
    assert_eq!(columns[1].ty, ColumnType::Array(Box::new(ColumnType::Text)));
    assert_eq!(
        columns[2].ty,
        ColumnType::Array(Box::new(ColumnType::Integer))
    );
    assert_eq!(
        columns[3].ty,
        ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Integer))))
    );

    let result = exec(
        &engine,
        "SELECT tags, nums, matrix,
                array_length(matrix, 2) AS matrix_width,
                cardinality(matrix) AS matrix_cardinality
         FROM array_values WHERE id = 1",
    );
    assert_eq!(
        result.rows[0]["tags"],
        array(vec![Value::Str("1".into()), Value::Str("2".into())])
    );
    assert_eq!(
        result.rows[0]["nums"],
        array(vec![Value::Int(10), Value::Int(20)])
    );
    assert_eq!(
        result.rows[0]["matrix"],
        array(vec![
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(3), Value::Int(4)]),
        ])
    );
    assert_eq!(result.rows[0]["matrix_width"], Value::Int(2));
    assert_eq!(result.rows[0]["matrix_cardinality"], Value::Int(4));
    let result = exec(
        &engine,
        "SELECT data_type, udt_name
         FROM information_schema.columns
         WHERE table_name = 'array_values' AND column_name = 'matrix'",
    );
    assert_eq!(result.rows[0]["data_type"], Value::Str("ARRAY".into()));
    assert_eq!(result.rows[0]["udt_name"], Value::Str("_int4".into()));
    let result = exec(
        &engine,
        "SELECT attndims, atttypid
         FROM pg_catalog.pg_attribute
         WHERE attname = 'matrix'",
    );
    assert_eq!(result.rows[0]["attndims"], Value::Int(2));
    assert_eq!(result.rows[0]["atttypid"], Value::Int(1007));
    let result = exec(&engine, "SELECT COUNT(*) AS count FROM array_values");
    assert_eq!(result.rows[0]["count"], Value::Int(2));
}

#[test]
fn array_length_returns_length() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT array_length(ARRAY[1, 2, 3], 1) AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Int(3));
}

#[test]
fn cardinality_returns_array_length() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT cardinality(ARRAY[1, 2, 3]) AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Int(3));
}

#[test]
fn array_cat_concatenates() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT array_cat(ARRAY[1, 2], ARRAY[3, 4]) AS v FROM t WHERE id = 1",
    );
    assert_eq!(
        result.rows[0]["v"],
        array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ])
    );
}

#[test]
fn array_append_appends() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT array_append(ARRAY[1, 2], 3) AS v FROM t WHERE id = 1",
    );
    assert_eq!(
        result.rows[0]["v"],
        array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
}

#[test]
fn array_remove_removes_matching_values() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT array_remove(ARRAY[1, 2, 3, 2], 2) AS v FROM t WHERE id = 1",
    );
    assert_eq!(
        result.rows[0]["v"],
        array(vec![Value::Int(1), Value::Int(3)])
    );
}

#[test]
fn gen_random_uuid_returns_uuid_shaped_string() {
    let engine = engine_with_table();
    let result = exec(&engine, "SELECT gen_random_uuid() AS v FROM t WHERE id = 1");
    let Value::Str(v) = &result.rows[0]["v"] else {
        panic!("expected uuid string, got {:?}", result.rows[0]["v"]);
    };
    let parts: Vec<_> = v.split('-').collect();
    assert_eq!(
        parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
        vec![8, 4, 4, 4, 12]
    );
}

#[test]
fn uuid_column_round_trips_as_text() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE uuid_test (id SERIAL PRIMARY KEY, uid UUID)",
    );
    exec(
        &engine,
        "INSERT INTO uuid_test (uid) VALUES ('550e8400-e29b-41d4-a716-446655440000')",
    );
    let result = exec(&engine, "SELECT uid FROM uuid_test WHERE id = 1");
    assert_eq!(
        result.rows[0]["uid"],
        Value::Str("550e8400-e29b-41d4-a716-446655440000".into())
    );
}

#[test]
fn gen_random_uuid_returns_unique_values() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT gen_random_uuid() AS a, gen_random_uuid() AS b FROM t WHERE id = 1",
    );
    assert_ne!(result.rows[0]["a"], result.rows[0]["b"]);
}

#[test]
fn bytea_column_accepts_text_input() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE bin_test (id SERIAL PRIMARY KEY, data BYTEA)",
    );
    exec(&engine, "INSERT INTO bin_test (data) VALUES ('hello')");
    let result = exec(&engine, "SELECT data FROM bin_test WHERE id = 1");
    assert_ne!(result.rows[0]["data"], Value::Null);
}

#[test]
fn bytea_columns_read_text_as_postgresql_byteain_does() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE binary_values (id int PRIMARY KEY, b bytea)",
    );
    // An untyped literal or parameter reaches the column through `byteain`, whatever writes it.
    for statement in [
        "INSERT INTO binary_values VALUES (1, '\\x0102')",
        "INSERT INTO binary_values VALUES (2, '\\x01 02\t0a')",
        "INSERT INTO binary_values VALUES (3, '\\x')",
        "INSERT INTO binary_values VALUES (4, 'abc\\\\def')",
        "INSERT INTO binary_values VALUES (5, '\\001\\002')",
        "INSERT INTO binary_values SELECT 6, '\\xe282ac'",
        "INSERT INTO binary_values VALUES (7, 'plain')",
        "UPDATE binary_values SET b = '\\x0a' WHERE id = 7",
        "INSERT INTO binary_values VALUES (1, '\\x00') ON CONFLICT (id) DO UPDATE SET b = '\\x0b0c'",
        "MERGE INTO binary_values USING (SELECT 8 AS id) AS s ON binary_values.id = s.id WHEN NOT MATCHED THEN INSERT VALUES (s.id, '\\xff')",
    ] {
        exec(&engine, statement);
    }
    engine
        .sql(
            "INSERT INTO binary_values VALUES ($1, $2)",
            &[
                uqa_sql::SQLParam::Scalar(Value::Int(9)),
                uqa_sql::SQLParam::Scalar(Value::Str("\\x0d0e".into())),
            ],
        )
        .unwrap();
    // COPY's text format removes its own escapes before the value reaches `byteain`.
    engine
        .copy_from(
            "COPY binary_values FROM STDIN",
            b"10\t\\\\x0a0b\n11\tabc\\\\\\\\def\n".as_slice(),
        )
        .unwrap();
    let rows = exec(
        &engine,
        "SELECT id, encode(b, 'hex') AS hex FROM binary_values ORDER BY id",
    );
    let stored = rows
        .rows
        .iter()
        .map(|row| match (&row["id"], &row["hex"]) {
            (Value::Int(id), Value::Str(hex)) => (*id, hex.clone()),
            other => panic!("unexpected row {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        stored,
        [
            (1, "0b0c"),
            (2, "01020a"),
            (3, ""),
            (4, "6162635c646566"),
            (5, "0102"),
            (6, "e282ac"),
            (7, "0a"),
            (8, "ff"),
            (9, "0d0e"),
            (10, "0a0b"),
            (11, "6162635c646566"),
        ]
        .map(|(id, hex)| (id, hex.to_string()))
    );
    for (statement, sqlstate, message) in [
        (
            "INSERT INTO binary_values VALUES (20, '\\x0')",
            "22023",
            "invalid hexadecimal data: odd number of digits",
        ),
        (
            "INSERT INTO binary_values VALUES (20, '\\xGG')",
            "22023",
            "invalid hexadecimal digit: \"G\"",
        ),
        (
            "INSERT INTO binary_values VALUES (20, '\\8')",
            "22P02",
            "invalid input syntax for type bytea",
        ),
        (
            "UPDATE binary_values SET b = 'a\\' WHERE id = 1",
            "22P02",
            "invalid input syntax for type bytea",
        ),
        (
            "INSERT INTO binary_values VALUES (20, 12)",
            "42804",
            "column \"b\" is of type bytea but expression is of type integer",
        ),
    ] {
        let error = engine.sql(statement, &[]).unwrap_err();
        assert_eq!(
            (error.sqlstate(), error.to_string().as_str()),
            (Some(sqlstate), message),
            "{statement}"
        );
    }
}

#[test]
fn encode_and_decode_use_postgresql_text_formats() {
    let engine = Engine::new();
    let row = exec(
        &engine,
        "SELECT encode(decode('01 02\t0a', 'hex'), 'hex') AS hex, \
                encode(decode('ABcd', 'HEX'), 'hex') AS upper, \
                encode(decode('a\\\\b\\001', 'escape'), 'hex') AS unescaped, \
                encode('\\x5c0001ff7f41'::bytea, 'escape') AS escaped, \
                encode('\\x0102ff'::bytea, 'base64') AS base64, \
                encode(decode(' AQL/\n', 'base64'), 'hex') AS spaced",
    );
    let text = |column: &str| match &row.rows[0][column] {
        Value::Str(text) => text.clone(),
        other => panic!("{column}: {other:?}"),
    };
    assert_eq!(text("hex"), "01020a");
    assert_eq!(text("upper"), "abcd");
    assert_eq!(text("unescaped"), "615c6201");
    assert_eq!(text("escaped"), "\\\\\\000\u{1}\\377\u{7f}A");
    assert_eq!(text("base64"), "AQL/");
    assert_eq!(text("spaced"), "0102ff");
    for (statement, sqlstate, message) in [
        (
            "SELECT decode('zz', 'hex')",
            "22023",
            "invalid hexadecimal digit: \"z\"",
        ),
        (
            "SELECT decode('0', 'hex')",
            "22023",
            "invalid hexadecimal data: odd number of digits",
        ),
        (
            "SELECT decode('a\\8', 'escape')",
            "22P02",
            "invalid input syntax for type bytea",
        ),
        (
            "SELECT decode('A?==', 'base64')",
            "22023",
            "invalid symbol \"?\" found while decoding base64 sequence",
        ),
        (
            "SELECT decode('AQ', 'base64')",
            "22023",
            "invalid base64 end sequence",
        ),
        (
            "SELECT decode('ab', 'nope')",
            "22023",
            "unrecognized encoding: \"nope\"",
        ),
        (
            "SELECT encode('\\x01'::bytea, 'nope')",
            "22023",
            "unrecognized encoding: \"nope\"",
        ),
    ] {
        let error = engine.sql(statement, &[]).unwrap_err();
        assert_eq!(
            (error.sqlstate(), error.to_string().as_str()),
            (Some(sqlstate), message),
            "{statement}"
        );
    }
}

#[test]
fn cast_text_to_bytea_returns_bytes() {
    let engine = engine_with_table();
    let result = exec(&engine, "SELECT 'hello'::bytea AS v FROM t WHERE id = 1");
    assert_eq!(result.rows[0]["v"], Value::Bytes(b"hello".to_vec()));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn multidimensional_array_input_matches_postgresql_and_survives_reopen(#[case] provider: usize) {
    let open = |path: &std::path::Path| match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("multidimensional.db");
    let engine = open(&path);
    let transcript = include_str!(
        "../../../tests/parity/pg18/multidimensional_array_input_oracle.expected.json"
    );
    crate::pg18_oracle::verify(&engine, transcript);
    if provider != 0 {
        drop(engine);
        let engine = open(&path);
        let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
        restored["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| case["id"] == "reopen_values");
        assert_eq!(restored["cases"].as_array().unwrap().len(), 1);
        crate::pg18_oracle::verify(&engine, &restored.to_string());
    }
}
