//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public legacy-vector behavior against independently captured `PostgreSQL` 18 results.

use std::{path::Path, sync::Arc};
use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};

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

fn oracle(ty: &str) -> serde_json::Value {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../uqa-core/src/types/tests/pg18_legacy_vectors.json"
    ))
    .unwrap();
    assert!(oracle["postgresql"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    oracle["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["type"] == ty)
        .unwrap()
        .clone()
}

fn rows(result: &SQLResult) -> serde_json::Value {
    serde_json::Value::Array(
        result
            .rows
            .iter()
            .enumerate()
            .map(|(position, row)| {
                serde_json::Value::Array(
                    result
                        .columns
                        .iter()
                        .enumerate()
                        .map(|(index, column)| {
                            let value = result
                                .positional_rows
                                .as_ref()
                                .map_or(&row[column], |rows| &rows[position][index]);
                            match value {
                                Value::Null => serde_json::Value::Null,
                                Value::Int(value) => (*value).into(),
                                Value::Bool(value) => (*value).into(),
                                Value::Str(value) => value.clone().into(),
                                other => panic!("unexpected oracle result value: {other:?}"),
                            }
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}

fn execute(engine: &Engine, sql: &str) -> SQLResult {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

fn verify_queries(engine: &Engine, oracle: &serde_json::Value, prepared: bool) {
    for case in oracle["queries"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let result = if prepared {
            execute(engine, &format!("PREPARE vector_query AS {sql}"));
            let result = execute(engine, "EXECUTE vector_query");
            execute(engine, "DEALLOCATE vector_query");
            result
        } else {
            execute(engine, sql)
        };
        assert_eq!(rows(&result), case["rows"], "{sql}");
    }
}

#[rstest::rstest]
fn legacy_vector_queries_match_pg18_before_and_after_indexes_and_reopen(
    #[values(0, 1, 2)] provider: usize,
    #[values("int2vector", "oidvector")] ty: &str,
    #[values(false, true)] prepared: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vectors.db");
    let engine = open(provider, &path);
    let oracle = oracle(ty);
    for sql in oracle["setup"].as_array().unwrap() {
        execute(&engine, sql.as_str().unwrap());
    }
    verify_queries(&engine, &oracle, prepared);
    execute(&engine, "CREATE INDEX vector_value ON legacy_values(value)");
    verify_queries(&engine, &oracle, prepared);
    execute(
        &engine,
        "PREPARE vector_parameter(legacy_items) AS SELECT id FROM legacy_values WHERE value=$1 ORDER BY id",
    );
    for cast in [ty, "legacy_items"] {
        assert_eq!(
            rows(&execute(
                &engine,
                &format!("EXECUTE vector_parameter('1 2'::{cast})")
            )),
            oracle["queries"][1]["rows"]
        );
    }
    drop(engine);
    let reopened = open(provider, &path);
    verify_queries(&reopened, &oracle, prepared);
    for case in oracle["rejected"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let error = reopened.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), case["sqlstate"].as_str(), "{sql}");
        if let Some(message) = case["message"].as_str() {
            assert_eq!(error.to_string(), message, "{sql}");
        }
    }
    for update in oracle["updates"].as_array().unwrap() {
        execute(&reopened, update["sql"].as_str().unwrap());
        verify_queries(&reopened, update, prepared);
    }
    drop(reopened);
    verify_queries(
        &open(provider, &path),
        oracle["updates"].as_array().unwrap().last().unwrap(),
        prepared,
    );
}

#[rstest::rstest]
fn legacy_vector_index_errors_match_pg18(
    #[values(0, 1, 2)] provider: usize,
    #[values("int2vector", "oidvector")] ty: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unique.db");
    let engine = open(provider, &path);
    for case in oracle(ty)["index_errors"].as_array().unwrap() {
        execute(&engine, "BEGIN");
        for sql in case["setup"].as_array().unwrap() {
            execute(&engine, sql.as_str().unwrap());
        }
        let sql = case["sql"].as_str().unwrap();
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), case["sqlstate"].as_str(), "{sql}");
        if let Some(message) = case["message"].as_str() {
            assert_eq!(error.to_string(), message, "{sql}");
        }
        execute(&engine, "ROLLBACK");
    }
}

#[rstest::rstest]
fn legacy_vector_conflict_update_retains_unchanged_index_inputs(
    #[values(0, 1, 2)] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("conflict.db"));
    execute(
        &engine,
        "CREATE TABLE vectors(id integer PRIMARY KEY,v oidvector,untouched integer)",
    );
    execute(
        &engine,
        "INSERT INTO vectors VALUES(1,trim_array('1'::oidvector,1),0)",
    );
    execute(&engine, "CREATE INDEX vector_key ON vectors(v)");
    let result = execute(&engine, "INSERT INTO vectors VALUES(1,'1',7) ON CONFLICT(id) DO UPDATE SET untouched=excluded.untouched RETURNING id,array_ndims(v),untouched");
    assert_eq!(rows(&result), serde_json::json!([[1, null, 7]]));
    let error = engine
        .sql(
            "INSERT INTO vectors VALUES(1,'1',8) ON CONFLICT(id) DO UPDATE SET id=2",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(error.to_string(), "array is not a valid oidvector");
    assert_eq!(
        rows(&execute(
            &engine,
            "SELECT id,array_ndims(v),untouched FROM vectors"
        )),
        serde_json::json!([[1, null, 7]])
    );
}

#[rstest::rstest]
#[case(false, "v")]
#[case(true, "tenant,v")]
#[case(true, "tenant,(v::oidvector)")]
fn legacy_vector_partition_validation_excludes_the_existing_row_identity(
    #[values(0, 1, 2)] provider: usize,
    #[case] unique: bool,
    #[case] keys: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("partition.db"));
    execute(
        &engine,
        "CREATE TABLE vectors(tenant integer,v oidvector) PARTITION BY RANGE(tenant)",
    );
    execute(
        &engine,
        &format!(
            "CREATE {} INDEX vector_key ON vectors({keys})",
            if unique { "UNIQUE" } else { "" }
        ),
    );
    execute(
        &engine,
        "CREATE TABLE candidate(tenant integer,v oidvector)",
    );
    execute(
        &engine,
        "INSERT INTO candidate VALUES(1,trim_array('1'::oidvector,1))",
    );
    execute(
        &engine,
        "ALTER TABLE vectors ATTACH PARTITION candidate FOR VALUES FROM(0) TO(10)",
    );
    assert_eq!(
        rows(&execute(
            &engine,
            "SELECT tenant,array_ndims(v) FROM vectors"
        )),
        serde_json::json!([[1, null]])
    );
    let error = engine
        .sql("INSERT INTO candidate VALUES(1,'1')", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(error.to_string(), "array is not a valid oidvector");
}
