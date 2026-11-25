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

#[test]
fn overlaps_binds_timestamp_literals_parameters_and_declared_text_like_postgresql() {
    let engine = engine();
    engine.sql("SET TIME ZONE 'America/New_York'", &[]).unwrap();
    let period = "(timestamptz '2000-01-01 05:05Z', timestamptz '2000-01-01 05:10Z')";
    for (endpoints, expected) in [
        ("('2000-01-01 00:00', '2000-01-01 00:15')", true),
        (
            "(timestamptz '2000-01-01 05:00Z', '2000-01-01 00:15')",
            true,
        ),
        ("('2000-01-01 00:00+00', '2000-01-01 00:15+00')", false),
        ("('2000-01-01 00:00-05', '2000-01-01 00:15-05')", true),
    ] {
        let result = engine
            .sql(&format!("SELECT {period} OVERLAPS {endpoints} AS v"), &[])
            .unwrap();
        assert_eq!(result.rows[0]["v"], Value::Bool(expected), "{endpoints}");
    }
    let error = engine
        .sql(
            &format!("SELECT {period} OVERLAPS ('2000-01-01'::text, '2000-01-02'::text)"),
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    engine.sql(&format!("PREPARE zoned_period(timestamptz) AS SELECT {period} OVERLAPS ($1, interval '15 minutes') AS v"), &[]).unwrap();
    let execute = "EXECUTE zoned_period('2000-01-01 00:00')";
    assert_eq!(
        engine.sql(execute, &[]).unwrap().rows[0]["v"],
        Value::Bool(true)
    );
    engine.sql("SET TIME ZONE 'UTC'", &[]).unwrap();
    assert_eq!(
        engine.sql(execute, &[]).unwrap().rows[0]["v"],
        Value::Bool(false)
    );
}

#[test]
fn generated_periods_require_immutable_overlaps_overloads() {
    let engine = engine();
    for (ty, end, allowed) in [
        ("timestamptz", "duration", false),
        ("timestamptz", "finish", true),
        ("timestamp", "duration", true),
    ] {
        let sql = format!(
            "CREATE TEMP TABLE period_{ty}_{end} (start_at {ty}, finish {ty}, duration interval,
             observed boolean GENERATED ALWAYS AS ((start_at, finish) OVERLAPS (start_at, {end})) STORED)"
        );
        let result = engine.sql(&sql, &[]);
        if allowed {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().sqlstate(), Some("42P17"));
        }
    }
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
