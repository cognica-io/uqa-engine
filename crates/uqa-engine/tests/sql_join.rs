//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INNER and LEFT JOIN with ON predicates.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_execution::DEFAULT_BATCH_SIZE;

fn corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT, dept_id INTEGER, salary INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE departments (id INTEGER PRIMARY KEY, name TEXT, location TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO employees (id, name, dept_id, salary) VALUES \
         (1, 'alice', 10, 90000), \
         (2, 'bob', 20, 75000), \
         (3, 'carol', 10, 110000), \
         (4, 'dave', 30, 60000)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO departments (id, name, location) VALUES \
         (10, 'engineering', 'sf'), \
         (20, 'sales', 'ny')",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn inner_join_pairs_matching_rows() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT e.name AS emp, d.name AS dept \
             FROM employees AS e \
             INNER JOIN departments AS d ON e.dept_id = d.id \
             ORDER BY e.id",
            &[],
        )
        .unwrap();
    let names: Vec<(String, String)> = r
        .rows
        .iter()
        .filter_map(|row| match (row.get("emp"), row.get("dept")) {
            (Some(Value::Str(e)), Some(Value::Str(d))) => Some((e.clone(), d.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("alice".into(), "engineering".into()),
            ("bob".into(), "sales".into()),
            ("carol".into(), "engineering".into()),
        ]
    );
}

#[test]
fn left_join_pads_unmatched_with_null() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT e.name AS emp, d.name AS dept \
             FROM employees AS e \
             LEFT JOIN departments AS d ON e.dept_id = d.id \
             ORDER BY e.id",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 4);
    assert_eq!(r.rows[3].get("emp"), Some(&Value::Str("dave".into())));
    assert_eq!(r.rows[3].get("dept"), Some(&Value::Null));
}

#[test]
fn join_with_where_filters_pairs() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT e.name AS emp \
             FROM employees AS e \
             INNER JOIN departments AS d ON e.dept_id = d.id \
             WHERE e.salary > 80000 \
             ORDER BY e.id",
            &[],
        )
        .unwrap();
    let emps: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("emp") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(emps, vec!["alice".to_string(), "carol".to_string()]);
}

#[test]
fn join_with_aggregate_groups_per_department() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT d.name AS dept, count(*) AS n, avg(e.salary) AS avg_sal \
             FROM employees AS e \
             INNER JOIN departments AS d ON e.dept_id = d.id \
             GROUP BY d.name \
             ORDER BY d.name",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    let eng_row = &r.rows[0];
    assert_eq!(eng_row.get("dept"), Some(&Value::Str("engineering".into())));
    assert_eq!(eng_row.get("n"), Some(&Value::Int(2)));
    match eng_row.get("avg_sal") {
        Some(Value::Float(v)) => assert!((v - 100_000.0).abs() < 1e-9),
        other => panic!("expected float avg, got {other:?}"),
    }
}

#[test]
fn pushed_filter_does_not_treat_an_empty_storage_page_as_eof() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE scan_rows (id BIGINT PRIMARY KEY)", &[])
        .unwrap();
    let last = DEFAULT_BATCH_SIZE + 1;
    eng.sql(
        &format!("INSERT INTO scan_rows SELECT n FROM generate_series(1, {last}) AS values(n)"),
        &[],
    )
    .unwrap();

    let result = eng
        .sql(
            &format!("SELECT s.id FROM scan_rows AS s WHERE s.id > {DEFAULT_BATCH_SIZE}"),
            &[],
        )
        .unwrap();

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(last as i64)));
}
