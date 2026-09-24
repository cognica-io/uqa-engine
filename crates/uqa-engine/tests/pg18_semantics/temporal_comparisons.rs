//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public temporal values, indexes and uniqueness follow independently captured `PostgreSQL` output.

use super::*;
use serde_json::Value as JSONValue;
use std::{path::Path, sync::Arc};

fn fixture() -> JSONValue {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-core/src/types/tests/pg18_temporal.json"
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
fn temporal_comparisons_match_postgresql_values_and_prepared_parameters() {
    let engine = engine();
    let oracle = fixture();
    let mut differences = Vec::new();
    for group in oracle["types"].as_array().unwrap() {
        let kind = group["kind"].as_str().unwrap();
        engine.sql(&format!("PREPARE temporal_compare({kind},{kind}) AS SELECT $1 = $2 AS eq,$1 < $2 AS lt,$1 > $2 AS gt"), &[]).unwrap();
        for pair in group["comparisons"].as_array().unwrap() {
            let left = group["values"][pair[0].as_u64().unwrap() as usize]
                .as_str()
                .unwrap();
            let right = group["values"][pair[1].as_u64().unwrap() as usize]
                .as_str()
                .unwrap();
            for sql in [format!("SELECT {kind} '{left}' = {kind} '{right}' AS eq,{kind} '{left}' < {kind} '{right}' AS lt,{kind} '{left}' > {kind} '{right}' AS gt"), format!("EXECUTE temporal_compare('{left}','{right}')")] {
                match engine.sql(&sql, &[]) {
                    Ok(result) => {
                        for (index, column) in ["eq", "lt", "gt"].into_iter().enumerate() {
                            let expected = Value::Bool(pair[index+2].as_bool().unwrap());
                            if result.rows[0][column] != expected {
                                differences.push(format!("{sql}: {column} expected {expected:?}, got {:?}", result.rows[0][column]));
                            }
                        }
                        assert_eq!(result.column_types, vec![Some(uqa_sql::ColumnType::Boolean);3]);
                    }
                    Err(error) => differences.push(format!("{sql}: {error}")),
                }
            }
        }
        engine.sql("DEALLOCATE temporal_compare", &[]).unwrap();
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

fn verify(engine: &Engine, oracle: &JSONValue) {
    let mut differences = Vec::new();
    for case in oracle["queries"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        match engine.sql(sql, &[]) {
            Ok(result) => {
                let rows: Vec<Vec<_>> = result
                    .rows
                    .iter()
                    .map(|row| result.columns.iter().map(|column| &row[column]).collect())
                    .collect();
                let actual = serde_json::to_value(rows).unwrap();
                if actual != case["rows"] {
                    differences.push(format!("{sql}: expected {}, got {actual}", case["rows"]));
                }
            }
            Err(error) => differences.push(format!("{sql}: {error}")),
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn temporal_comparisons_preserve_scan_index_group_join_uniqueness_and_reopen() {
    let oracle = fixture();
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("temporal.db");
        {
            let engine = open(provider, &path);
            for sql in oracle["setup"].as_array().unwrap() {
                engine.sql(sql.as_str().unwrap(), &[]).unwrap();
            }
            verify(&engine, &oracle);
            for kind in ["time", "timetz"] {
                engine
                    .sql(
                        &format!("CREATE INDEX temporal_{kind}_v ON temporal_{kind}(v)"),
                        &[],
                    )
                    .unwrap();
            }
            verify(&engine, &oracle);
        }
        let engine = open(provider, &path);
        verify(&engine, &oracle);
        for group in oracle["unique"].as_array().unwrap() {
            for sql in group["setup"].as_array().unwrap() {
                engine.sql(sql.as_str().unwrap(), &[]).unwrap();
            }
            for case in group["commands"].as_array().unwrap() {
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
}
