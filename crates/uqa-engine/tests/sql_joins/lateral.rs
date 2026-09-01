//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn lateral_subquery_with_aggregate() {
    let engine = lateral_engine();
    let result = query(
        &engine,
        "SELECT d.dept_name, sub.top_salary \
         FROM depts d, \
         LATERAL (SELECT MAX(salary) AS top_salary \
         FROM emps WHERE emps.dept_id = d.id) sub \
         ORDER BY d.dept_name",
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        result.rows[0]["dept_name"],
        Value::Str("Engineering".into())
    );
    assert_eq!(result.rows[0]["top_salary"], Value::Int(90000));
    assert_eq!(result.rows[1]["dept_name"], Value::Str("Sales".into()));
    assert_eq!(result.rows[1]["top_salary"], Value::Int(75000));
}

#[test]
fn lateral_with_limit() {
    let engine = lateral_engine();
    let result = query(
        &engine,
        "SELECT d.dept_name, sub.top_emp, sub.top_sal \
         FROM depts d, \
         LATERAL (SELECT emp_name AS top_emp, salary AS top_sal \
         FROM emps WHERE emps.dept_id = d.id \
         ORDER BY salary DESC LIMIT 1) sub \
         ORDER BY d.dept_name",
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["top_emp"], Value::Str("Alice".into()));
    assert_eq!(result.rows[0]["top_sal"], Value::Int(90000));
    assert_eq!(result.rows[1]["top_emp"], Value::Str("Diana".into()));
    assert_eq!(result.rows[1]["top_sal"], Value::Int(75000));
}

#[test]
fn lateral_with_count() {
    let engine = lateral_engine();
    let result = query(
        &engine,
        "SELECT d.dept_name, sub.emp_count \
         FROM depts d, \
         LATERAL (SELECT COUNT(*) AS emp_count \
         FROM emps WHERE emps.dept_id = d.id) sub \
         ORDER BY d.dept_name",
    );
    assert_eq!(
        result.rows[0]["dept_name"],
        Value::Str("Engineering".into())
    );
    assert_eq!(result.rows[0]["emp_count"], Value::Int(2));
    assert_eq!(result.rows[1]["dept_name"], Value::Str("Sales".into()));
    assert_eq!(result.rows[1]["emp_count"], Value::Int(2));
}

#[test]
fn lateral_subqueries_preserve_outer_and_output_type_identity() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE lateral_types (v SMALLINT, label VARCHAR(7), score REAL)",
    );
    exec(
        &engine,
        "INSERT INTO lateral_types VALUES (7, 'seven', 1.5)",
    );

    let outer_types = query(
        &engine,
        "SELECT s.v_type, s.label_type, s.score_type
         FROM lateral_types AS l
         CROSS JOIN LATERAL (
             SELECT pg_typeof(l.v) AS v_type,
                    pg_typeof(l.label) AS label_type,
                    pg_typeof(l.score) AS score_type
         ) AS s",
    );
    assert_eq!(outer_types.rows[0]["v_type"], Value::Str("smallint".into()));
    assert_eq!(
        outer_types.rows[0]["label_type"],
        Value::Str("character varying".into())
    );
    assert_eq!(outer_types.rows[0]["score_type"], Value::Str("real".into()));

    let output_types = query(
        &engine,
        "SELECT s.v, s.label, s.score
         FROM lateral_types AS l
         CROSS JOIN LATERAL (
             SELECT l.v::bigint AS v,
                    l.label::varchar(3) AS label,
                    l.score::double precision AS score
         ) AS s",
    );
    assert_eq!(
        output_types.column_types,
        [
            Some(uqa_sql::ColumnType::BigInteger),
            Some(uqa_sql::ColumnType::Varchar(Some(3))),
            Some(uqa_sql::ColumnType::DoublePrecision),
        ]
    );
}

#[test]
fn empty_cte_lateral_source_keeps_its_declared_type() {
    let engine = Engine::new();
    let result = query(
        &engine,
        "WITH c AS (SELECT 1::smallint AS v WHERE false)
         SELECT pg_typeof(s.v) AS ty, s.v
         FROM (VALUES (1)) AS seed(n)
         LEFT JOIN LATERAL (SELECT v FROM c) AS s ON true",
    );
    assert_eq!(result.rows[0]["ty"], Value::Str("smallint".into()));
    assert_eq!(result.rows[0]["v"], Value::Null);
    assert_eq!(
        result.column_types,
        [
            Some(uqa_sql::ColumnType::Regtype),
            Some(uqa_sql::ColumnType::SmallInteger),
        ]
    );
}
