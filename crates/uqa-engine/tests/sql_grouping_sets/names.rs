//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/semantics/grouping_sets/pg18_names.json"
    )))
    .unwrap()
}

fn populate(engine: &Engine) {
    for sql in fixture()["setup"].as_array().unwrap() {
        engine.sql(sql.as_str().unwrap(), &[]).unwrap();
    }
}

fn integer_rows(engine: &Engine, sql: &str) -> Vec<Vec<i64>> {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result
        .rows
        .iter()
        .map(|row| {
            result
                .columns
                .iter()
                .map(|column| {
                    let Value::Int(value) = row[column] else {
                        panic!("integer expected: {row:?}: {sql}")
                    };
                    value
                })
                .collect()
        })
        .collect()
}

#[test]
fn grouping_names_match_postgresql_rows_and_errors() {
    let engine = Engine::new();
    populate(&engine);
    let mut differences = Vec::new();
    for case in fixture()["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        match (case["sqlstate"].as_str(), engine.sql(sql, &[])) {
            (Some(state), Err(error)) if error.sqlstate() == Some(state) => {}
            (expected, Err(error)) => differences.push(format!(
                "{sql}: expected {}, got {error:?}",
                expected.unwrap_or("rows")
            )),
            (Some(state), Ok(_)) => differences.push(format!("{sql}: expected {state}, got rows")),
            (None, Ok(result)) => {
                let rows = result
                    .rows
                    .iter()
                    .map(|row| {
                        result
                            .columns
                            .iter()
                            .map(|column| row[column].clone())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                let actual = serde_json::to_value(rows).unwrap();
                if actual != case["rows"] {
                    differences.push(format!("{sql}: expected {}, got {actual}", case["rows"]));
                }
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn grouping_names_bind_before_prepared_parameter_analysis() {
    let engine = Engine::new();
    populate(&engine);
    engine.sql("PREPARE grouping_lookup(integer) AS SELECT min(id) AS first_id,count(*) AS n FROM grouping_alias_probe WHERE id > $1 GROUP BY n ORDER BY first_id", &[]).unwrap();
    assert_eq!(
        integer_rows(&engine, "EXECUTE grouping_lookup(0)"),
        [vec![1, 2], vec![2, 1]]
    );
}

#[test]
fn grouping_names_in_stored_views_survive_column_rename_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("grouping.db");
    {
        let engine = Engine::open(&path).unwrap();
        populate(&engine);
        engine.sql("CREATE VIEW input_group AS SELECT min(id) AS first_id,count(*) AS n FROM grouping_alias_probe GROUP BY n", &[]).unwrap();
        engine.sql("CREATE VIEW output_group AS SELECT n + 1 AS shifted,count(*) AS tally FROM grouping_alias_probe GROUP BY shifted", &[]).unwrap();
        engine
            .sql(
                "ALTER TABLE grouping_alias_probe RENAME COLUMN n TO value",
                &[],
            )
            .unwrap();
    }
    let engine = Engine::open(&path).unwrap();
    assert_eq!(
        integer_rows(&engine, "SELECT * FROM input_group ORDER BY first_id"),
        [vec![1, 2], vec![2, 1]]
    );
    assert_eq!(
        integer_rows(&engine, "SELECT * FROM output_group ORDER BY shifted"),
        [vec![11, 2], vec![21, 1]]
    );
}

#[test]
fn grouping_names_prefer_output_columns_to_enclosing_columns() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE enclosing (p int)",
        "INSERT INTO enclosing VALUES (5)",
        "CREATE TABLE grouped (v int)",
        "INSERT INTO grouped VALUES (1), (2), (3)",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    assert_eq!(
        integer_rows(
            &engine,
            "SELECT (SELECT count(*) FROM (SELECT v AS p FROM grouped GROUP BY p) s) AS c FROM enclosing",
        ),
        vec![vec![3]]
    );
    assert_eq!(
        integer_rows(
            &engine,
            "SELECT x.p, x.n FROM enclosing, LATERAL (SELECT v % 2 AS p, count(*) AS n FROM grouped GROUP BY p ORDER BY p) x",
        ),
        vec![vec![0, 1], vec![1, 2]]
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn repeated_scalar_query_aliases_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("clause-query-bindings.db");
    let open = || match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/binding/tests/pg18_clause_query_results.json"
    )))
    .unwrap();
    let verify = |engine: &Engine| {
        for case in reference["cases"].as_array().unwrap() {
            let sql = case["sql"].as_str().unwrap();
            let result = engine.sql(sql, &[]);
            if let Some(expected) = case.get("error") {
                let error = result.unwrap_err();
                assert_eq!(
                    error.sqlstate(),
                    expected["sqlstate"].as_str(),
                    "{sql}: {error}"
                );
                assert_eq!(
                    error.to_string(),
                    expected["message"].as_str().unwrap(),
                    "{sql}"
                );
            } else {
                let result = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
                assert_eq!(result.columns, ["value"], "{sql}");
                let expected = case["csv"]
                    .as_str()
                    .unwrap()
                    .lines()
                    .map(|value| match case["columns"][0][1].as_str().unwrap() {
                        "integer" => Value::Int(value.parse().unwrap()),
                        "text" => Value::Str(value.into()),
                        other => panic!("unsupported fixture type: {other}"),
                    })
                    .collect::<Vec<_>>();
                let actual = result
                    .rows
                    .iter()
                    .map(|row| row["value"].clone())
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "{sql}");
            }
        }
    };
    let engine = open();
    engine.sql("CREATE TABLE input(x integer)", &[]).unwrap();
    engine.sql("INSERT INTO input VALUES(7)", &[]).unwrap();
    verify(&engine);
    if provider != 0 {
        drop(engine);
        verify(&open());
    }
}
