//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Group-expression identities compared with independently captured `PostgreSQL` results.

use super::*;
use std::{path::Path, sync::Arc};

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

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/semantics/aggregates/pg18_literals.json"
    )))
    .unwrap()
}

fn text_rows(result: &uqa_engine::SQLResult) -> serde_json::Value {
    let rows: Vec<Vec<Option<String>>> = (0..result.rows.len())
        .map(|row| {
            (0..result.columns.len())
                .map(|column| {
                    let value = result.value_at(row, column).unwrap();
                    if matches!(value, Value::Null) {
                        None
                    } else {
                        Some(
                            uqa_sql::result::format_postgres_text(
                                value,
                                result.column_types[column].as_ref().unwrap(),
                                None,
                            )
                            .unwrap(),
                        )
                    }
                })
                .collect()
        })
        .collect();
    serde_json::to_value(rows).unwrap()
}

#[rstest::rstest]
fn grouping_literals_match_postgresql_rows_and_errors(
    #[values(0, 1, 2)] provider: usize,
    #[values(0, 1, 2)] mode: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("grouping.db");
    let engine = open(provider, &path);
    let fixture = fixture();
    for sql in fixture["setup"].as_array().unwrap() {
        engine.sql(sql.as_str().unwrap(), &[]).unwrap();
    }
    drop(engine);
    let engine = open(provider, &path);
    engine
        .sql(
            match mode {
                0 => "SET plan_cache_mode = auto",
                1 => "SET plan_cache_mode = force_custom_plan",
                2 => "SET plan_cache_mode = force_generic_plan",
                _ => unreachable!(),
            },
            &[],
        )
        .unwrap();
    let mut differences = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        engine.sql("DEALLOCATE ALL", &[]).unwrap();
        let name = case["name"].as_str().unwrap();
        let sql = case["sql"].as_str().unwrap();
        let sql = if mode == 0 || sql.starts_with("PREPARE ") {
            sql.to_owned()
        } else {
            format!("PREPARE literal_probe AS {sql}; EXECUTE literal_probe")
        };
        match (case["sqlstate"].as_str(), engine.sql(&sql, &[])) {
            (Some(state), Err(error))
                if error.sqlstate() == Some(state)
                    && error.to_string() == case["message"].as_str().unwrap() => {}
            (expected, Err(error)) => differences.push(format!(
                "{name}: expected {}: {}, got {error:?}",
                expected.unwrap_or("rows"),
                case["message"]
            )),
            (Some(state), Ok(_)) => differences.push(format!("{name}: expected {state}, got rows")),
            (None, Ok(result)) => {
                let rows = text_rows(&result);
                let columns = serde_json::to_value(&result.columns).unwrap();
                if rows != case["rows"] || columns != case["columns"] {
                    differences.push(format!(
                        "{name}: expected {} {}, got {columns} {rows}",
                        case["columns"], case["rows"]
                    ));
                }
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[rstest::rstest]
fn stored_grouping_literals_survive_column_rename_and_reopen(#[values(0, 1, 2)] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("grouping_view.db");
    let engine = open(provider, &path);
    let fixture = fixture();
    for sql in fixture["setup"].as_array().unwrap() {
        engine.sql(sql.as_str().unwrap(), &[]).unwrap();
    }
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "having_group_expression")
        .unwrap();
    engine
        .sql(
            &format!(
                "CREATE VIEW stored_literal_groups AS {}",
                case["sql"].as_str().unwrap()
            ),
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE grouping_literal_probe RENAME COLUMN n TO number",
            &[],
        )
        .unwrap();
    drop(engine);
    let engine = open(provider, &path);
    let result = engine
        .sql("SELECT * FROM stored_literal_groups ORDER BY shifted", &[])
        .unwrap();
    assert_eq!(text_rows(&result), case["rows"]);
}
