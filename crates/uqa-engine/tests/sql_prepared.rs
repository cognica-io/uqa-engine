//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared-statement coverage.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};
use uqa_sql::{ColumnType, SQLParam};

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn err(engine: &Engine, sql: &str) -> String {
    engine.sql(sql, &[]).unwrap_err().to_string()
}

fn setup() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE employees (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            dept TEXT,
            salary REAL
        )",
    );
    exec(
        &engine,
        "INSERT INTO employees (id, name, dept, salary) VALUES
            (1, 'Alice', 'eng', 90000),
            (2, 'Bob', 'mkt', 75000),
            (3, 'Carol', 'eng', 85000),
            (4, 'Dave', 'sales', 70000),
            (5, 'Eve', 'eng', 95000)",
    );
    engine
}

#[test]
fn prepare_select_registers_statement() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_by_id (INTEGER) AS SELECT name FROM employees WHERE id = $1",
    );
    assert!(engine.lookup_prepared("get_by_id").is_some());
}

#[test]
fn direct_parameters_retain_static_type_during_projection_binding() {
    let engine = Engine::new();
    let result = engine
        .sql(
            "SELECT pg_typeof($1) AS ty",
            &[SQLParam::Scalar(Value::Int(1))],
        )
        .unwrap();
    assert_eq!(result.rows[0]["ty"], Value::Str("integer".into()));
    assert_eq!(result.column_types, [Some(ColumnType::Regtype)]);
}

#[test]
fn operator_and_builtin_results_use_postgresql_return_types() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT
            '2020-01-01'::date + 1 AS next_date,
            '2020-01-02'::date - '2020-01-01'::date AS elapsed_days,
            '2020-01-02'::timestamp - '2020-01-01'::timestamp AS elapsed_time,
            CRC32('abc'::bytea) AS checksum,
            REGEXP_MATCH('abc', '(b)') AS captures,
            ARRAY_REVERSE(ARRAY[1::smallint, 2::smallint]) AS reversed,
            CURRENT_DATABASE() AS database_name,
            UUIDV4() AS generated_uuid",
    );
    assert_eq!(
        result.column_types,
        [
            Some(ColumnType::Date),
            Some(ColumnType::Integer),
            Some(ColumnType::Interval),
            Some(ColumnType::BigInteger),
            Some(ColumnType::Array(Box::new(ColumnType::Text))),
            Some(ColumnType::Array(Box::new(ColumnType::SmallInteger))),
            Some(ColumnType::Name),
            Some(ColumnType::Uuid),
        ]
    );
}

#[test]
fn user_function_declarations_bind_empty_and_nested_result_types() {
    let engine = Engine::new();
    exec(&engine, "CREATE SCHEMA typed_functions");
    exec(
        &engine,
        "CREATE FUNCTION typed_functions.pick(value SMALLINT) RETURNS SMALLINT
         LANGUAGE SQL IMMUTABLE AS 'SELECT value'",
    );
    exec(
        &engine,
        "CREATE FUNCTION typed_functions.pick(value INTEGER) RETURNS BIGINT
         LANGUAGE SQL IMMUTABLE AS 'SELECT value::bigint'",
    );
    exec(
        &engine,
        "CREATE FUNCTION typed_functions.named(value INTEGER, extra BIGINT DEFAULT 0)
         RETURNS REAL LANGUAGE SQL IMMUTABLE AS 'SELECT value::real'",
    );

    let result = exec(
        &engine,
        "SELECT
            typed_functions.pick(1::smallint) AS small_result,
            typed_functions.pick(1::integer) AS big_result,
            COALESCE(typed_functions.pick(NULL::smallint), 2::smallint) AS nested_result,
            typed_functions.named(extra => 1::bigint, value => 1::integer) AS named_result
         WHERE FALSE",
    );
    assert!(result.rows.is_empty());
    assert_eq!(
        result.column_types,
        [
            Some(ColumnType::SmallInteger),
            Some(ColumnType::BigInteger),
            Some(ColumnType::SmallInteger),
            Some(ColumnType::Real),
        ]
    );
}

#[test]
fn result_and_cursor_expose_statically_bound_output_types() {
    let engine = Engine::new();
    let sql = "SELECT
        1::smallint AS small_value,
        2::bigint AS big_value,
        3::real AS real_value,
        4::double precision AS double_value,
        'x'::varchar(5) AS varying_value,
        '550e8400-e29b-41d4-a716-446655440000'::uuid AS uuid_value";
    let expected = vec![
        Some(ColumnType::SmallInteger),
        Some(ColumnType::BigInteger),
        Some(ColumnType::Real),
        Some(ColumnType::DoublePrecision),
        Some(ColumnType::Varchar(Some(5))),
        Some(ColumnType::Uuid),
    ];
    let result = engine.sql(sql, &[]).unwrap();
    assert_eq!(result.column_types, expected);

    let cursor = engine.sql_cursor(sql, &[]).unwrap();
    assert_eq!(cursor.column_types(), expected);
}

#[test]
fn aggregate_results_preserve_postgresql_return_types() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE aggregate_types (
            small_value SMALLINT,
            big_value BIGINT,
            real_value REAL
        )",
    );
    exec(
        &engine,
        "INSERT INTO aggregate_types VALUES (1, 10, 1.5), (2, 20, 2.5)",
    );
    let result = exec(
        &engine,
        "SELECT
            COUNT(*) AS count_value,
            SUM(small_value) AS small_sum,
            AVG(small_value) AS small_avg,
            SUM(big_value) AS big_sum,
            AVG(real_value) AS real_avg,
            MIN(small_value) AS small_min,
            ARRAY_AGG(small_value) AS small_array
         FROM aggregate_types",
    );
    assert_eq!(
        result.column_types,
        [
            Some(ColumnType::BigInteger),
            Some(ColumnType::BigInteger),
            Some(ColumnType::Numeric {
                precision: None,
                scale: None,
            }),
            Some(ColumnType::Numeric {
                precision: None,
                scale: None,
            }),
            Some(ColumnType::DoublePrecision),
            Some(ColumnType::SmallInteger),
            Some(ColumnType::Array(Box::new(ColumnType::SmallInteger))),
        ]
    );
    let types = exec(
        &engine,
        "SELECT
            pg_typeof(COUNT(*)) AS count_type,
            pg_typeof(SUM(small_value)) AS small_sum_type,
            pg_typeof(AVG(small_value)) AS small_avg_type,
            pg_typeof(SUM(big_value)) AS big_sum_type,
            pg_typeof(AVG(real_value)) AS real_avg_type,
            pg_typeof(MIN(small_value)) AS small_min_type,
            pg_typeof(ARRAY_AGG(small_value)) AS small_array_type
         FROM aggregate_types",
    );
    assert_eq!(types.rows[0]["count_type"], Value::Str("bigint".into()));
    assert_eq!(types.rows[0]["small_sum_type"], Value::Str("bigint".into()));
    assert_eq!(
        types.rows[0]["small_avg_type"],
        Value::Str("numeric".into())
    );
    assert_eq!(types.rows[0]["big_sum_type"], Value::Str("numeric".into()));
    assert_eq!(
        types.rows[0]["real_avg_type"],
        Value::Str("double precision".into())
    );
    assert_eq!(
        types.rows[0]["small_min_type"],
        Value::Str("smallint".into())
    );
    assert_eq!(
        types.rows[0]["small_array_type"],
        Value::Str("smallint[]".into())
    );
}

#[test]
fn window_results_preserve_postgresql_return_types_through_spill() {
    let engine = Engine::new();
    engine
        .set_variable("work_mem", "1kB")
        .expect("set tiny window spill budget");
    exec(
        &engine,
        "CREATE TABLE window_types (small_value SMALLINT, real_value REAL)",
    );
    exec(
        &engine,
        "INSERT INTO window_types VALUES (1, 1.5), (2, 2.5), (3, 3.5)",
    );
    let result = exec(
        &engine,
        "SELECT
            small_value,
            ROW_NUMBER() OVER (ORDER BY small_value) AS row_index,
            LAG(small_value) OVER (ORDER BY small_value) AS previous_value,
            SUM(small_value) OVER () AS total_value,
            AVG(real_value) OVER () AS average_value
         FROM window_types
         ORDER BY small_value",
    );
    assert_eq!(
        result.column_types,
        [
            Some(ColumnType::SmallInteger),
            Some(ColumnType::BigInteger),
            Some(ColumnType::SmallInteger),
            Some(ColumnType::BigInteger),
            Some(ColumnType::DoublePrecision),
        ]
    );
}

#[test]
fn prepare_duplicate_raises() {
    let engine = setup();
    exec(&engine, "PREPARE q AS SELECT name FROM employees");
    assert!(err(&engine, "PREPARE q AS SELECT name FROM employees").contains("already exists"));
}

#[test]
fn prepare_insert_registers_statement() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE ins AS
         INSERT INTO employees (id, name, dept, salary)
         VALUES ($1, $2, $3, $4)",
    );
    assert!(engine.lookup_prepared("ins").is_some());
}

#[test]
fn prepare_update_registers_statement() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE upd AS UPDATE employees SET salary = $1 WHERE id = $2",
    );
    assert!(engine.lookup_prepared("upd").is_some());
}

#[test]
fn prepare_delete_registers_statement() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE del AS DELETE FROM employees WHERE id = $1",
    );
    assert!(engine.lookup_prepared("del").is_some());
}

#[test]
fn execute_select_single_param() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_by_id AS SELECT name FROM employees WHERE id = $1",
    );
    let result = exec(&engine, "EXECUTE get_by_id (1)");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["name"], Value::Str("Alice".into()));
}

#[test]
fn execute_select_different_params() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_by_id AS SELECT name FROM employees WHERE id = $1",
    );
    assert_eq!(
        exec(&engine, "EXECUTE get_by_id (1)").rows[0]["name"],
        Value::Str("Alice".into())
    );
    assert_eq!(
        exec(&engine, "EXECUTE get_by_id (3)").rows[0]["name"],
        Value::Str("Carol".into())
    );
}

#[test]
fn execute_select_multiple_params() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_by_dept_sal AS
         SELECT name FROM employees
         WHERE dept = $1 AND salary > $2
         ORDER BY name",
    );
    let result = exec(&engine, "EXECUTE get_by_dept_sal ('eng', 87000)");
    let names: Vec<_> = result.rows.iter().map(|r| r["name"].clone()).collect();
    assert_eq!(
        names,
        vec![Value::Str("Alice".into()), Value::Str("Eve".into())]
    );
}

#[test]
fn execute_insert() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE ins AS
         INSERT INTO employees (id, name, dept, salary)
         VALUES ($1, $2, $3, $4)",
    );
    exec(&engine, "EXECUTE ins (6, 'Frank', 'mkt', 80000)");
    let result = exec(&engine, "SELECT name FROM employees WHERE id = 6");
    assert_eq!(result.rows[0]["name"], Value::Str("Frank".into()));
}

#[test]
fn execute_update() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE upd AS UPDATE employees SET salary = $1 WHERE id = $2",
    );
    exec(&engine, "EXECUTE upd (100000, 1)");
    let result = exec(&engine, "SELECT salary FROM employees WHERE id = 1");
    assert_eq!(result.rows[0]["salary"], Value::Float(100_000.0));
}

#[test]
fn execute_delete() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE del AS DELETE FROM employees WHERE id = $1",
    );
    exec(&engine, "EXECUTE del (4)");
    let result = exec(&engine, "SELECT COUNT(*) AS cnt FROM employees");
    assert_eq!(result.rows[0]["cnt"], Value::Int(4));
}

#[test]
fn execute_nonexistent_raises() {
    let engine = setup();
    assert!(err(&engine, "EXECUTE nonexistent (1)").contains("does not exist"));
}

#[test]
fn execute_missing_param_raises() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE q AS SELECT name FROM employees WHERE id = $1 AND dept = $2",
    );
    assert!(err(&engine, "EXECUTE q (1)").contains("No value supplied"));
}

#[test]
fn execute_reusable() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_name AS SELECT name FROM employees WHERE id = $1",
    );
    let mut names = Vec::new();
    for i in 1..=5 {
        let result = exec(&engine, &format!("EXECUTE get_name ({i})"));
        names.push(result.rows[0]["name"].clone());
    }
    assert_eq!(
        names,
        vec![
            Value::Str("Alice".into()),
            Value::Str("Bob".into()),
            Value::Str("Carol".into()),
            Value::Str("Dave".into()),
            Value::Str("Eve".into()),
        ]
    );
}

#[test]
fn deallocate_removes_statement() {
    let engine = setup();
    exec(&engine, "PREPARE q AS SELECT name FROM employees");
    exec(&engine, "DEALLOCATE q");
    assert!(engine.lookup_prepared("q").is_none());
}

#[test]
fn deallocate_nonexistent_raises() {
    let engine = setup();
    assert!(err(&engine, "DEALLOCATE nonexistent").contains("does not exist"));
}

#[test]
fn deallocate_all_removes_every_statement() {
    let engine = setup();
    exec(&engine, "PREPARE q1 AS SELECT name FROM employees");
    exec(&engine, "PREPARE q2 AS SELECT dept FROM employees");
    exec(&engine, "DEALLOCATE ALL");
    assert!(engine.lookup_prepared("q1").is_none());
    assert!(engine.lookup_prepared("q2").is_none());
}

#[test]
fn execute_after_deallocate_raises() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE q AS SELECT name FROM employees WHERE id = $1",
    );
    exec(&engine, "DEALLOCATE q");
    assert!(err(&engine, "EXECUTE q (1)").contains("does not exist"));
}

#[test]
fn reprepare_after_deallocate() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE q AS SELECT name FROM employees WHERE dept = $1",
    );
    exec(&engine, "DEALLOCATE q");
    exec(
        &engine,
        "PREPARE q AS SELECT salary FROM employees WHERE id = $1",
    );
    let result = exec(&engine, "EXECUTE q (1)");
    assert_eq!(result.rows[0]["salary"], Value::Float(90_000.0));
}

#[test]
fn prepare_with_typed_params() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE q (INTEGER) AS SELECT name FROM employees WHERE id = $1",
    );
    let result = exec(&engine, "EXECUTE q (2)");
    assert_eq!(result.rows[0]["name"], Value::Str("Bob".into()));
}

#[test]
fn prepare_select_with_order_and_limit() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE top_earners AS
         SELECT name, salary FROM employees
         WHERE dept = $1 ORDER BY salary DESC LIMIT 2",
    );
    let result = exec(&engine, "EXECUTE top_earners ('eng')");
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["name"], Value::Str("Eve".into()));
    assert_eq!(result.rows[1]["name"], Value::Str("Alice".into()));
}

#[test]
fn prepare_select_no_params() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE all_names AS SELECT name FROM employees ORDER BY name",
    );
    let result = exec(&engine, "EXECUTE all_names");
    let names: Vec<_> = result.rows.iter().map(|r| r["name"].clone()).collect();
    assert_eq!(
        names,
        vec![
            Value::Str("Alice".into()),
            Value::Str("Bob".into()),
            Value::Str("Carol".into()),
            Value::Str("Dave".into()),
            Value::Str("Eve".into()),
        ]
    );
}

#[test]
fn prepare_with_null_param() {
    let engine = setup();
    exec(
        &engine,
        "INSERT INTO employees (id, name, dept, salary) VALUES (6, 'Frank', NULL, 80000)",
    );
    exec(
        &engine,
        "PREPARE get_null_dept AS SELECT name FROM employees WHERE dept IS NULL",
    );
    let result = exec(&engine, "EXECUTE get_null_dept");
    assert_eq!(result.rows[0]["name"], Value::Str("Frank".into()));
}

#[test]
fn multiple_prepared_coexist() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE by_id AS SELECT name FROM employees WHERE id = $1",
    );
    exec(
        &engine,
        "PREPARE by_dept AS
         SELECT name FROM employees WHERE dept = $1 ORDER BY name",
    );
    let r1 = exec(&engine, "EXECUTE by_id (1)");
    let r2 = exec(&engine, "EXECUTE by_dept ('eng')");
    assert_eq!(r1.rows[0]["name"], Value::Str("Alice".into()));
    let dept_names: Vec<_> = r2.rows.iter().map(|r| r["name"].clone()).collect();
    assert_eq!(
        dept_names,
        vec![
            Value::Str("Alice".into()),
            Value::Str("Carol".into()),
            Value::Str("Eve".into()),
        ]
    );
}
