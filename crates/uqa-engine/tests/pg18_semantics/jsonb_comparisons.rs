//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSONB comparison paths use independently captured `PostgreSQL` outcomes.

use super::*;
use serde_json::Value as JSONValue;
use std::{path::Path, sync::Arc};

fn fixture() -> JSONValue {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-core/src/types/tests/pg18_jsonb.json"
    )))
    .unwrap()
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

#[test]
fn jsonb_comparisons_match_postgresql_values_and_prepared_parameters() {
    let engine = engine();
    let oracle = fixture();
    engine.sql("PREPARE jsonb_compare(jsonb,jsonb) AS SELECT $1 = $2 AS eq,$1 < $2 AS lt,$1 > $2 AS gt", &[]).unwrap();
    for pair in oracle["comparisons"].as_array().unwrap() {
        let left = oracle["values"][pair[0].as_u64().unwrap() as usize]
            .as_str()
            .unwrap()
            .replace('\'', "''");
        let right = oracle["values"][pair[1].as_u64().unwrap() as usize]
            .as_str()
            .unwrap()
            .replace('\'', "''");
        for sql in [
            format!("SELECT '{left}'::jsonb = '{right}'::jsonb AS eq,'{left}'::jsonb < '{right}'::jsonb AS lt,'{left}'::jsonb > '{right}'::jsonb AS gt"),
            format!("EXECUTE jsonb_compare('{left}','{right}')"),
        ] {
            let result = engine.sql(&sql, &[]).unwrap_or_else(|error| panic!("{sql}: {error}"));
            for (index, column) in ["eq", "lt", "gt"].into_iter().enumerate() {
                assert_eq!(result.rows[0][column], Value::Bool(pair[index + 2].as_bool().unwrap()), "{sql}: {column}");
            }
            assert_eq!(result.column_types, vec![Some(uqa_sql::ColumnType::Boolean); 3]);
        }
    }
}

fn verify(engine: &Engine, oracle: &JSONValue) {
    for case in oracle["queries"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let result = engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let rows: Vec<Vec<_>> = result
            .rows
            .iter()
            .map(|row| result.columns.iter().map(|column| &row[column]).collect())
            .collect();
        assert_eq!(serde_json::to_value(rows).unwrap(), case["rows"], "{sql}");
    }
}

#[test]
fn jsonb_comparisons_preserve_scan_index_group_join_uniqueness_and_reopen() {
    let oracle = fixture();
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jsonb.db");
        {
            let engine = open(provider, &path);
            for sql in oracle["setup"].as_array().unwrap() {
                engine.sql(sql.as_str().unwrap(), &[]).unwrap();
            }
            verify(&engine, &oracle);
            engine
                .sql(
                    "CREATE INDEX jsonb_comparison_v ON jsonb_comparison(v)",
                    &[],
                )
                .unwrap();
            verify(&engine, &oracle);
        }
        let engine = open(provider, &path);
        verify(&engine, &oracle);
        for case in oracle["unique"].as_array().unwrap() {
            let sql = case["sql"].as_str().unwrap();
            let result = engine.sql(sql, &[]);
            if let Some(state) = case["sqlstate"].as_str() {
                assert_eq!(
                    result.unwrap_err().sqlstate(),
                    Some(state),
                    "provider {provider}: {sql}"
                );
            } else {
                result.unwrap_or_else(|error| panic!("provider {provider}: {sql}: {error}"));
            }
        }
    }
}
