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
