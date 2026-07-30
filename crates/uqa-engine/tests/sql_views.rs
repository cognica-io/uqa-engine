//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_views`.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult, SQLScalarFunction};
use uqa_sql::SQLError;

struct CountCalls {
    calls: Arc<AtomicUsize>,
}

impl SQLScalarFunction for CountCalls {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        if !args.is_empty() {
            return Err(SQLError::BadArity {
                name: "count_calls".into(),
                expected: "0 arguments".into(),
                actual: args.len(),
            });
        }
        let value = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Value::Int(value as i64))
    }
}

struct ObserveValue {
    calls: Arc<AtomicUsize>,
}

impl SQLScalarFunction for ObserveValue {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        let [value] = args else {
            return Err(SQLError::BadArity {
                name: "observe_value".into(),
                expected: "1 argument".into(),
                actual: args.len(),
            });
        };
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(value.clone())
    }
}

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn engine() -> Engine {
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
            (5, 'Eve', 'eng', 95000),
            (6, 'Frank', 'mkt', 80000)",
    );
    engine
}

fn names(result: &SQLResult) -> Vec<String> {
    result
        .rows
        .iter()
        .map(|row| match &row["name"] {
            Value::Str(s) => s.clone(),
            other => panic!("expected name string, got {other:?}"),
        })
        .collect()
}

#[test]
fn create_view_basic() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW eng_employees AS
         SELECT name, salary FROM employees WHERE dept = 'eng'",
    );
    assert!(engine.view("eng_employees").unwrap().is_some());
}

#[test]
fn create_view_duplicate_raises() {
    let engine = engine();
    exec(&engine, "CREATE VIEW v AS SELECT name FROM employees");
    assert!(engine
        .sql("CREATE VIEW v AS SELECT name FROM employees", &[])
        .is_err());
}

#[test]
fn create_view_name_conflicts_with_table() {
    let engine = engine();
    assert!(engine
        .sql("CREATE VIEW employees AS SELECT name FROM employees", &[])
        .is_err());
}

#[test]
fn select_all_from_view() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW eng AS SELECT name, salary FROM employees WHERE dept = 'eng'",
    );
    let r = exec(&engine, "SELECT name FROM eng ORDER BY name");
    assert_eq!(names(&r), vec!["Alice", "Carol", "Eve"]);
}

#[test]
fn view_with_filter() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW high_sal AS
         SELECT name, salary FROM employees WHERE salary > 80000",
    );
    let r = exec(&engine, "SELECT name FROM high_sal WHERE salary > 90000");
    assert_eq!(names(&r), vec!["Eve"]);
}

#[test]
fn view_with_aggregate() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW dept_stats AS
         SELECT dept, COUNT(*) AS cnt, AVG(salary) AS avg_sal
         FROM employees GROUP BY dept",
    );
    let r = exec(&engine, "SELECT dept, cnt FROM dept_stats ORDER BY dept");
    assert_eq!(r.rows[0]["dept"], Value::Str("eng".into()));
    assert_eq!(r.rows[0]["cnt"], Value::Int(3));
}

#[test]
fn view_with_order_and_limit() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW ranked AS
         SELECT name, salary FROM employees ORDER BY salary DESC",
    );
    let r = exec(&engine, "SELECT name FROM ranked LIMIT 3");
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0]["name"], Value::Str("Eve".into()));
}

#[test]
fn view_preserves_column_types() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name, salary FROM employees",
    );
    let r = exec(&engine, "SELECT salary FROM v WHERE name = 'Alice'");
    assert_eq!(r.rows[0]["salary"], Value::Float(90_000.0));
}

#[test]
fn view_with_distinct() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW depts AS SELECT DISTINCT dept FROM employees",
    );
    let r = exec(&engine, "SELECT dept FROM depts ORDER BY dept");
    let got: Vec<_> = r
        .rows
        .iter()
        .map(|row| match &row["dept"] {
            Value::Str(s) => s.clone(),
            other => panic!("expected dept string, got {other:?}"),
        })
        .collect();
    assert_eq!(got, vec!["eng", "mkt", "sales"]);
}

#[test]
fn view_does_not_leak_temp_table() {
    let engine = engine();
    exec(&engine, "CREATE VIEW v AS SELECT name FROM employees");
    exec(&engine, "SELECT name FROM v");
    assert!(!engine.has_table("v").unwrap());
    assert!(engine.view("v").unwrap().is_some());
}

#[test]
fn view_does_not_shadow_real_table() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name FROM employees LIMIT 1",
    );
    exec(&engine, "SELECT name FROM v");
    let r = exec(&engine, "SELECT COUNT(*) AS cnt FROM employees");
    assert_eq!(r.rows[0]["cnt"], Value::Int(6));
}

#[test]
fn multiple_view_queries() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name, salary FROM employees",
    );
    let r1 = exec(&engine, "SELECT COUNT(*) AS cnt FROM v");
    let r2 = exec(&engine, "SELECT name FROM v WHERE salary > 90000");
    assert_eq!(r1.rows[0]["cnt"], Value::Int(6));
    assert_eq!(names(&r2), vec!["Eve"]);
}

#[test]
fn drop_view() {
    let engine = engine();
    exec(&engine, "CREATE VIEW v AS SELECT name FROM employees");
    exec(&engine, "DROP VIEW v");
    assert!(engine.view("v").unwrap().is_none());
}

#[test]
fn drop_view_if_exists() {
    let engine = engine();
    exec(&engine, "DROP VIEW IF EXISTS nonexistent");
}

#[test]
fn drop_view_nonexistent_raises() {
    let engine = engine();
    assert!(engine.sql("DROP VIEW nonexistent", &[]).is_err());
}

#[test]
fn drop_view_then_select_raises() {
    let engine = engine();
    exec(&engine, "CREATE VIEW v AS SELECT name FROM employees");
    exec(&engine, "DROP VIEW v");
    assert!(engine.sql("SELECT name FROM v", &[]).is_err());
}

#[test]
fn recreate_view_after_drop() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name FROM employees WHERE dept = 'eng'",
    );
    exec(&engine, "DROP VIEW v");
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name FROM employees WHERE dept = 'mkt'",
    );
    let r = exec(&engine, "SELECT name FROM v ORDER BY name");
    assert_eq!(names(&r), vec!["Bob", "Frank"]);
}

#[test]
fn view_reflects_data_changes() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name, salary FROM employees",
    );
    exec(
        &engine,
        "INSERT INTO employees (id, name, dept, salary) VALUES (7, 'Grace', 'eng', 100000)",
    );
    let r = exec(&engine, "SELECT COUNT(*) AS cnt FROM v");
    assert_eq!(r.rows[0]["cnt"], Value::Int(7));
}

#[test]
fn view_with_window_function() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW ranked AS
         SELECT name, salary, ROW_NUMBER() OVER (ORDER BY salary DESC) AS rn
         FROM employees",
    );
    let r = exec(
        &engine,
        "SELECT name, rn FROM ranked WHERE rn <= 3 ORDER BY rn",
    );
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0]["name"], Value::Str("Eve".into()));
}

#[test]
fn view_used_in_subquery() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW eng_ids AS SELECT id FROM employees WHERE dept = 'eng'",
    );
    let r = exec(
        &engine,
        "SELECT name FROM employees
         WHERE id IN (SELECT id FROM eng_ids)
         ORDER BY name",
    );
    assert_eq!(names(&r), vec!["Alice", "Carol", "Eve"]);
}

#[test]
fn view_of_view() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW high_sal AS
         SELECT name, salary FROM employees WHERE salary > 80000",
    );
    exec(
        &engine,
        "CREATE VIEW very_high AS SELECT name FROM high_sal WHERE salary > 90000",
    );
    let r = exec(&engine, "SELECT name FROM very_high");
    assert_eq!(names(&r), vec!["Eve"]);
}

#[test]
fn cte_and_view_together() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW eng AS SELECT name, salary FROM employees WHERE dept = 'eng'",
    );
    let r = exec(
        &engine,
        "WITH top AS (SELECT name FROM eng WHERE salary > 90000)
         SELECT name FROM top",
    );
    assert_eq!(names(&r), vec!["Eve"]);
}

#[test]
fn volatile_registered_callback_view_is_not_cached_per_statement() {
    let engine = engine();
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_scalar_function(
            "count_calls",
            CountCalls {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();
    exec(
        &engine,
        "CREATE VIEW counted AS SELECT count_calls() AS marker",
    );

    let r = exec(
        &engine,
        "SELECT a.marker AS left_marker, b.marker AS right_marker
         FROM counted a CROSS JOIN counted b",
    );

    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["left_marker"], Value::Int(1));
    assert_eq!(r.rows[0]["right_marker"], Value::Int(2));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let r = exec(&engine, "SELECT marker FROM counted");
    assert_eq!(r.rows[0]["marker"], Value::Int(3));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn volatile_dependency_is_transitive_through_nested_views() {
    let engine = engine();
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_scalar_function(
            "count_calls",
            CountCalls {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();
    exec(
        &engine,
        "CREATE VIEW counted AS SELECT count_calls() AS marker",
    );
    exec(
        &engine,
        "CREATE VIEW left_counted AS SELECT marker FROM counted",
    );
    exec(
        &engine,
        "CREATE VIEW right_counted AS SELECT marker FROM counted",
    );

    let r = exec(
        &engine,
        "SELECT l.marker AS left_marker, r.marker AS right_marker
         FROM left_counted l CROSS JOIN right_counted r",
    );

    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["left_marker"], Value::Int(1));
    assert_eq!(r.rows[0]["right_marker"], Value::Int(2));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn filtered_nested_views_do_not_cache_volatile_dependency() {
    let engine = engine();
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_scalar_function(
            "count_calls",
            CountCalls {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();
    exec(
        &engine,
        "CREATE VIEW counted AS SELECT DISTINCT 1 AS id, count_calls() AS marker",
    );
    exec(
        &engine,
        "CREATE VIEW left_counted AS SELECT id, marker FROM counted",
    );
    exec(
        &engine,
        "CREATE VIEW right_counted AS SELECT id, marker FROM counted",
    );

    let r = exec(
        &engine,
        "SELECT l.marker AS left_marker, r.marker AS right_marker
         FROM left_counted l CROSS JOIN right_counted r
         WHERE l.id = 1 AND r.id = 1",
    );

    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["left_marker"], Value::Int(1));
    assert_eq!(r.rows[0]["right_marker"], Value::Int(2));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn volatile_projection_is_not_duplicated_by_outer_filter_pushdown() {
    let engine = engine();
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_scalar_function(
            "observe_value",
            ObserveValue {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();
    exec(
        &engine,
        "CREATE VIEW observed AS
         SELECT id, observe_value(id) AS marker FROM employees",
    );

    let result = exec(
        &engine,
        "SELECT id FROM observed WHERE marker > 0 ORDER BY id",
    );

    assert_eq!(result.rows.len(), 6);
    assert_eq!(calls.load(Ordering::SeqCst), 6);
}

#[test]
fn declared_volatile_sql_function_is_not_hidden_by_view_cache() {
    let engine = engine();
    exec(&engine, "CREATE SEQUENCE volatile_view_seq START 1");
    exec(
        &engine,
        "CREATE FUNCTION volatile_view_tick() RETURNS BIGINT AS $$
             SELECT nextval('volatile_view_seq')
         $$ LANGUAGE sql VOLATILE",
    );
    exec(
        &engine,
        "CREATE VIEW volatile_ticks AS
         SELECT volatile_view_tick() AS marker",
    );

    let result = exec(
        &engine,
        "SELECT a.marker AS left_marker, b.marker AS right_marker
         FROM volatile_ticks a CROSS JOIN volatile_ticks b",
    );

    assert_eq!(result.rows[0]["left_marker"], Value::Int(1));
    assert_eq!(result.rows[0]["right_marker"], Value::Int(2));
}

#[test]
fn view_sequence_literals_bind_to_creation_namespace_and_block_drop() {
    let engine = Engine::new();
    exec(&engine, "CREATE SCHEMA s1");
    exec(&engine, "CREATE SCHEMA s2");
    exec(&engine, "CREATE SEQUENCE s1.ids START 10");
    exec(&engine, "CREATE SEQUENCE s2.ids START 100");
    exec(&engine, "SET search_path TO s1");
    exec(
        &engine,
        "CREATE VIEW public.sequence_values AS
         SELECT nextval('ids') AS next_value,
                currval('ids') AS current_value,
                setval('ids', 41) AS set_value",
    );

    let mut plan = engine.view("public.sequence_values").unwrap().unwrap();
    let mut references = Vec::new();
    plan.rewrite_scalar_expressions(&mut |expression| {
        let uqa_execution::ScalarExpr::Func { name, args, .. } = expression else {
            return;
        };
        if matches!(name.as_str(), "nextval" | "currval" | "setval") {
            let Some(uqa_execution::ScalarExpr::Literal(Value::Str(reference))) = args.first()
            else {
                panic!("sequence function must retain a literal reference");
            };
            references.push(reference.clone());
        }
    });
    assert_eq!(references, ["s1.ids", "s1.ids", "s1.ids"]);

    exec(&engine, "SET search_path TO s2");
    let result = exec(
        &engine,
        "SELECT next_value, current_value, set_value FROM public.sequence_values",
    );
    assert_eq!(result.rows[0]["next_value"], Value::Int(10));
    assert_eq!(result.rows[0]["current_value"], Value::Int(10));
    assert_eq!(result.rows[0]["set_value"], Value::Int(41));
    assert_eq!(
        exec(&engine, "SELECT nextval('s1.ids') AS value").rows[0]["value"],
        Value::Int(42)
    );
    assert_eq!(
        exec(&engine, "SELECT nextval('ids') AS value").rows[0]["value"],
        Value::Int(100)
    );

    assert!(engine.drop_sequence("s2.ids").unwrap());
    let error = engine.drop_sequence("s1.ids").unwrap_err();
    assert!(error.contains("public.sequence_values"), "{error}");
}

#[test]
fn persisted_view_sequence_binding_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("view-sequence-binding.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "CREATE SCHEMA s1");
        exec(&engine, "CREATE SCHEMA s2");
        exec(&engine, "CREATE SEQUENCE s1.ids START 10");
        exec(&engine, "CREATE SEQUENCE s2.ids START 100");
        exec(&engine, "SET search_path TO s1");
        exec(
            &engine,
            "CREATE VIEW public.sequence_value AS SELECT nextval('ids') AS value",
        );
    }

    let engine = Engine::open(&path).unwrap();
    exec(&engine, "SET search_path TO s2");
    assert_eq!(
        exec(&engine, "SELECT value FROM public.sequence_value").rows[0]["value"],
        Value::Int(10)
    );
    assert_eq!(
        exec(&engine, "SELECT nextval('ids') AS value").rows[0]["value"],
        Value::Int(100)
    );
    let error = engine.drop_sequence("s1.ids").unwrap_err();
    assert!(error.contains("public.sequence_value"), "{error}");
}
