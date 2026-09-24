//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public comparison, key and persisted-index results captured independently from `PostgreSQL` 18.

use super::*;
use serde_json::Value as JSONValue;
use std::{path::Path, sync::Arc};

fn fixture() -> JSONValue {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/expr/binary/comparison/pg18.json"
    )))
    .unwrap()
}

#[test]
fn numeric_comparisons_match_postgresql_values_types_and_errors() {
    let engine = engine();
    let oracle = fixture();
    for case in oracle["cases"].as_array().unwrap() {
        for (index, operator) in oracle["operators"].as_array().unwrap().iter().enumerate() {
            let sql = format!(
                "SELECT ({}) {} ({}) AS result",
                case["left"].as_str().unwrap(),
                operator.as_str().unwrap(),
                case["right"].as_str().unwrap()
            );
            let result = engine.sql(&sql, &[]);
            if let Some(sqlstate) = case["sqlstate"].as_str() {
                assert_eq!(result.unwrap_err().sqlstate(), Some(sqlstate), "{sql}");
            } else {
                let result = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
                assert_eq!(
                    result.column_types,
                    [Some(uqa_sql::ColumnType::Boolean)],
                    "{sql}"
                );
                let expected = case["values"][index]
                    .as_bool()
                    .map_or(Value::Null, Value::Bool);
                assert_eq!(result.rows[0]["result"], expected, "{sql}");
            }
        }
    }
}

fn open(provider: usize, path: &Path) -> Engine {
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

fn verify_relations(engine: &Engine, oracle: &JSONValue) {
    for case in oracle["relations"]["queries"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let result = engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let rows: Vec<Vec<JSONValue>> = result
            .rows
            .iter()
            .map(|row| {
                result
                    .columns
                    .iter()
                    .map(|column| match &row[column] {
                        Value::Null => JSONValue::Null,
                        Value::Bool(value) => (*value).into(),
                        Value::Int(value) => (*value).into(),
                        other => panic!("unexpected reference result {other:?}: {sql}"),
                    })
                    .collect()
            })
            .collect();
        assert_eq!(serde_json::to_value(rows).unwrap(), case["rows"], "{sql}");
    }
    engine
        .sql(
            "PREPARE numeric_compare(bigint, double precision) AS SELECT $1 = $2 AS result",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "EXECUTE numeric_compare(9007199254740993, 9007199254740992)"
        ),
        Value::Bool(true)
    );
    engine.sql("DEALLOCATE numeric_compare", &[]).unwrap();
}

#[test]
fn numeric_comparisons_preserve_scan_index_group_join_and_reopen_results() {
    let oracle = fixture();
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("numeric.db");
        {
            let engine = open(provider, &path);
            for sql in oracle["relations"]["setup"].as_array().unwrap() {
                engine.sql(sql.as_str().unwrap(), &[]).unwrap();
            }
            verify_relations(&engine, &oracle);
            for field in ["i", "f", "n"] {
                engine
                    .sql(
                        &format!("CREATE INDEX numeric_{field} ON numeric_comparison({field})"),
                        &[],
                    )
                    .unwrap();
            }
            verify_relations(&engine, &oracle);
        }
        let engine = open(provider, &path);
        verify_relations(&engine, &oracle);
        for case in oracle["relations"]["unique"].as_array().unwrap() {
            let sql = case["sql"].as_str().unwrap();
            let result = engine.sql(sql, &[]);
            if let Some(sqlstate) = case["sqlstate"].as_str() {
                assert_eq!(result.unwrap_err().sqlstate(), Some(sqlstate), "{sql}");
            } else {
                result.unwrap_or_else(|error| panic!("{sql}: {error}"));
            }
        }
    }
}

#[test]
fn numeric_comparisons_preserve_search_filters_over_inherited_tables() {
    let engine = engine();
    for sql in [
        "CREATE TABLE comparison_parent(id integer, i bigint, body text)",
        "CREATE TABLE comparison_child() INHERITS(comparison_parent)",
        "CREATE INDEX comparison_body ON comparison_parent USING gin(body)",
        "CREATE INDEX comparison_child_body ON comparison_child USING gin(body)",
        "INSERT INTO comparison_parent VALUES(1,9007199254740992,'needle')",
        "INSERT INTO comparison_child VALUES(2,9007199254740993,'needle')",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    // The float8 equality is the same independently captured PostgreSQL comparison as the scalar fixture; both documents also match the text query.
    for filter in [
        "i = 9007199254740992::float8",
        "text_match(body,'needle') AND i = 9007199254740992::float8",
        "text_match(body,'needle') AND (i = 9007199254740992::float8 OR id = 0)",
    ] {
        let sql = format!("SELECT id FROM comparison_parent WHERE {filter} ORDER BY id");
        let result = engine.sql(&sql, &[]).unwrap();
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row["id"].clone())
                .collect::<Vec<_>>(),
            [Value::Int(1), Value::Int(2)],
            "{sql}"
        );
    }
}

#[test]
fn nonfinite_float_columns_and_arrays_survive_indexes_and_reopen() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nonfinite.db");
        {
            let engine = open(provider, &path);
            engine.sql("CREATE TABLE nonfinite_values(id integer PRIMARY KEY, f double precision, a double precision[])", &[]).unwrap();
            engine.sql("INSERT INTO nonfinite_values VALUES (1,'NaN',ARRAY['NaN'::float8]), (2,'Infinity',ARRAY['Infinity'::float8]), (3,'-Infinity',ARRAY['-Infinity'::float8]), (4,NULL,ARRAY[NULL::float8])", &[]).unwrap();
            engine
                .sql("CREATE INDEX nonfinite_index ON nonfinite_values(f)", &[])
                .unwrap();
        }
        let engine = open(provider, &path);
        let rows = engine.sql("SELECT id,f,f IS NULL AS missing,a[1] = f AS same FROM nonfinite_values ORDER BY id", &[]).unwrap().rows;
        for (position, row) in rows.iter().enumerate() {
            assert_eq!(row["missing"], Value::Bool(position == 3));
            assert_eq!(
                row["same"],
                if position == 3 {
                    Value::Null
                } else {
                    Value::Bool(true)
                }
            );
            if position < 3 {
                let Value::Float(value) = row["f"] else {
                    panic!("lost float carrier: provider {provider}, row {row:?}")
                };
                match position {
                    0 => assert!(value.is_nan()),
                    1 => assert_eq!(value, f64::INFINITY),
                    _ => assert_eq!(value, f64::NEG_INFINITY),
                }
            } else {
                assert_eq!(row["f"], Value::Null);
            }
        }
        assert_eq!(rows.len(), 4);
        assert_eq!(
            scalar(
                &engine,
                "SELECT id FROM nonfinite_values WHERE f = 'NaN'::float8"
            ),
            Value::Int(1)
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT id FROM nonfinite_values WHERE f = 'Infinity'::float8"
            ),
            Value::Int(2)
        );
        assert_eq!(
            scalar(
                &engine,
                "SELECT id FROM nonfinite_values WHERE f = '-Infinity'::float8"
            ),
            Value::Int(3)
        );
    }
}
