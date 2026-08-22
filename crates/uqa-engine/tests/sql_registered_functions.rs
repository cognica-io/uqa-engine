//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use uqa_core::Value;
use uqa_engine::{
    Engine, SQLAggregateState, SQLScalarFunction, SQLTableFunction, SQLTableFunctionResult,
    SQLTableFunctionStream,
};
use uqa_sql::{ast::ColumnType, SQLError};

struct Prefixer {
    prefix: String,
}

impl SQLScalarFunction for Prefixer {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        let [Value::Str(input)] = args else {
            return Err(SQLError::BadArity {
                name: "rust_prefix".into(),
                expected: "1 text argument".into(),
                actual: args.len(),
            });
        };
        Ok(Value::Str(format!("{}{input}", self.prefix)))
    }
}

#[test]
fn registered_scalar_function_runs_from_projection_and_filter() {
    let eng = Engine::new();
    eng.register_scalar_function(
        "rust_prefix",
        Prefixer {
            prefix: "tag:".into(),
        },
    )
    .unwrap();
    eng.sql("CREATE TABLE notes (body TEXT)", &[]).unwrap();
    eng.sql("INSERT INTO notes (body) VALUES ('alpha'), ('beta')", &[])
        .unwrap();

    let res = eng
        .sql(
            "SELECT rust_prefix(body) AS tagged FROM notes \
             WHERE rust_prefix(body) = 'tag:beta'",
            &[],
        )
        .unwrap();

    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0]["tagged"], Value::Str("tag:beta".into()));

    let cte_res = eng
        .sql(
            "WITH src AS (SELECT body FROM notes) \
             SELECT rust_prefix(body) AS tagged FROM src \
             WHERE rust_prefix(body) = 'tag:alpha'",
            &[],
        )
        .unwrap();
    assert_eq!(cte_res.rows.len(), 1);
    assert_eq!(cte_res.rows[0]["tagged"], Value::Str("tag:alpha".into()));
}

struct RepeatRows;

impl SQLTableFunction for RepeatRows {
    fn call(&self, args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        let [Value::Str(label), Value::Int(times)] = args else {
            return Err(SQLError::BadArity {
                name: "rust_repeat_rows".into(),
                expected: "text, integer".into(),
                actual: args.len(),
            });
        };
        let rows = (0..*times)
            .map(|idx| vec![Value::Str(label.clone()), Value::Int(idx)])
            .collect();
        Ok(SQLTableFunctionResult::new(["label", "idx"], rows))
    }
}

struct CountingRows {
    calls: Arc<AtomicUsize>,
}

impl SQLTableFunction for CountingRows {
    fn call(&self, args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        if !args.is_empty() {
            return Err(SQLError::BadArity {
                name: "rust_counting_rows".into(),
                expected: "0 arguments".into(),
                actual: args.len(),
            });
        }
        let value = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(SQLTableFunctionResult::new(
            ["marker"],
            vec![vec![Value::Int(value as i64)]],
        ))
    }
}

#[test]
fn registered_table_function_runs_in_from_with_column_aliases() {
    let eng = Engine::new();
    eng.register_table_function("rust_repeat_rows", RepeatRows)
        .unwrap();

    let res = eng
        .sql(
            "SELECT name, n FROM rust_repeat_rows('row', 3) AS r(name, n) ORDER BY n",
            &[],
        )
        .unwrap();

    assert_eq!(res.rows.len(), 3);
    assert_eq!(res.rows[0]["name"], Value::Str("row".into()));
    assert_eq!(res.rows[0]["n"], Value::Int(0));
    assert_eq!(res.rows[2]["n"], Value::Int(2));

    let ordinal = eng
        .sql(
            "SELECT name, n, sequence \
             FROM rust_repeat_rows('row', 3) WITH ORDINALITY AS r(name, n, sequence) \
             ORDER BY sequence",
            &[],
        )
        .unwrap();
    assert_eq!(ordinal.columns, ["name", "n", "sequence"]);
    assert_eq!(ordinal.column_types[2], Some(ColumnType::BigInteger));
    assert_eq!(ordinal.value_at(0, 2), Some(&Value::Int(1)));
    assert_eq!(ordinal.value_at(2, 2), Some(&Value::Int(3)));
}

#[test]
fn registered_table_function_source_makes_a_view_volatile() {
    let eng = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    eng.register_table_function(
        "rust_counting_rows",
        CountingRows {
            calls: Arc::clone(&calls),
        },
    )
    .unwrap();
    eng.sql(
        "CREATE VIEW counted_table_source AS
         SELECT marker FROM rust_counting_rows() AS r(marker)",
        &[],
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT a.marker AS left_marker, b.marker AS right_marker
             FROM counted_table_source a CROSS JOIN counted_table_source b",
            &[],
        )
        .unwrap();

    assert_eq!(result.rows[0]["left_marker"], Value::Int(1));
    assert_eq!(result.rows[0]["right_marker"], Value::Int(2));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

struct StreamingRows;

impl SQLTableFunction for StreamingRows {
    fn call_stream(&self, args: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
        let [Value::Int(count)] = args else {
            return Err(SQLError::BadArity {
                name: "rust_streaming_rows".into(),
                expected: "1 integer argument".into(),
                actual: args.len(),
            });
        };
        let count = *count;
        Ok(SQLTableFunctionStream::new(
            ["n"],
            (0..count).map(|value| Ok(vec![Value::Int(value)])),
        ))
    }
}

struct LateTableFunctionFailure;

impl SQLTableFunction for LateTableFunctionFailure {
    fn call_stream(&self, _args: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
        Ok(SQLTableFunctionStream::new(
            ["n"],
            std::iter::once(Ok(vec![Value::Int(1)])).chain(std::iter::once(Err(
                SQLError::Internal("late registered table-function failure".into()),
            ))),
        ))
    }
}

#[test]
fn registered_table_function_can_stream_under_tiny_work_mem() {
    let eng = Engine::new();
    eng.sql("SET work_mem TO '1B'", &[]).unwrap();
    eng.register_table_function("rust_streaming_rows", StreamingRows)
        .unwrap();

    let result = eng
        .sql(
            "SELECT count(*) AS total, max(n) AS maximum \
             FROM rust_streaming_rows(4096)",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["total"], Value::Int(4_096));
    assert_eq!(result.rows[0]["maximum"], Value::Int(4_095));
}

#[test]
fn registered_table_function_propagates_late_stream_errors() {
    let eng = Engine::new();
    eng.register_table_function("rust_late_failure", LateTableFunctionFailure)
        .unwrap();

    let error = eng
        .sql("SELECT * FROM rust_late_failure()", &[])
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("late registered table-function failure"));
}

#[derive(Default)]
struct SumSquares {
    total: i64,
}

impl SQLAggregateState for SumSquares {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        let [value] = args else {
            return Err(SQLError::BadArity {
                name: "rust_sum_squares".into(),
                expected: "1 numeric argument".into(),
                actual: args.len(),
            });
        };
        match value {
            Value::Int(n) => {
                self.total += n * n;
                Ok(())
            }
            Value::Null => Ok(()),
            other => Err(SQLError::TypeMismatch(format!(
                "rust_sum_squares expected integer, got {other:?}"
            ))),
        }
    }

    fn finish(&self) -> Result<Value, SQLError> {
        Ok(Value::Int(self.total))
    }
}

struct CountingSum {
    total: i64,
    finishes: Arc<AtomicUsize>,
}

impl SQLAggregateState for CountingSum {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        let [Value::Int(value)] = args else {
            return Err(SQLError::BadArity {
                name: "rust_counted_sum".into(),
                expected: "1 integer argument".into(),
                actual: args.len(),
            });
        };
        self.total += value;
        Ok(())
    }

    fn finish(&self) -> Result<Value, SQLError> {
        self.finishes.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Int(self.total))
    }
}

#[test]
fn registered_aggregate_function_participates_in_group_by() {
    let eng = Engine::new();
    eng.register_aggregate_function("rust_sum_squares", SumSquares::default)
        .unwrap();
    eng.sql("CREATE TABLE samples (grp TEXT, val INTEGER)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO samples (grp, val) VALUES \
         ('a', 1), ('a', 2), ('b', 3)",
        &[],
    )
    .unwrap();

    let res = eng
        .sql(
            "SELECT grp, RUST_SUM_SQUARES(val) AS total \
             FROM samples GROUP BY grp ORDER BY grp",
            &[],
        )
        .unwrap();

    assert_eq!(res.rows.len(), 2);
    assert_eq!(res.rows[0]["grp"], Value::Str("a".into()));
    assert_eq!(res.rows[0]["total"], Value::Int(5));
    assert_eq!(res.rows[1]["grp"], Value::Str("b".into()));
    assert_eq!(res.rows[1]["total"], Value::Int(9));
}

#[test]
fn registered_aggregate_makes_a_view_volatile() {
    let eng = Engine::new();
    let finishes = Arc::new(AtomicUsize::new(0));
    let state_finishes = Arc::clone(&finishes);
    eng.register_aggregate_function("rust_counted_sum", move || CountingSum {
        total: 0,
        finishes: Arc::clone(&state_finishes),
    })
    .unwrap();
    eng.sql("CREATE TABLE aggregate_inputs (value INTEGER)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO aggregate_inputs(value) VALUES (1), (2), (3)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE VIEW counted_aggregate AS
         SELECT rust_counted_sum(value) AS total FROM aggregate_inputs",
        &[],
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT a.total AS left_total, b.total AS right_total
             FROM counted_aggregate a CROSS JOIN counted_aggregate b",
            &[],
        )
        .unwrap();

    assert_eq!(result.rows[0]["left_total"], Value::Int(6));
    assert_eq!(result.rows[0]["right_total"], Value::Int(6));
    assert_eq!(finishes.load(Ordering::SeqCst), 2);
}

#[test]
fn prepared_plan_rebinds_when_aggregate_registry_changes() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE samples (val INTEGER)", &[]).unwrap();
    eng.sql("INSERT INTO samples (val) VALUES (1), (2)", &[])
        .unwrap();
    eng.sql(
        "PREPARE totals AS SELECT rust_sum_squares(val) AS total FROM samples",
        &[],
    )
    .unwrap();

    eng.register_aggregate_function("rust_sum_squares", SumSquares::default)
        .unwrap();
    let result = eng.sql("EXECUTE totals", &[]).unwrap();

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["total"], Value::Int(5));
}

#[derive(Default)]
struct JoinObserved {
    parts: Vec<String>,
}

impl SQLAggregateState for JoinObserved {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        let [Value::Str(value)] = args else {
            return Err(SQLError::BadArity {
                name: "rust_join_observed".into(),
                expected: "1 text argument".into(),
                actual: args.len(),
            });
        };
        self.parts.push(value.clone());
        Ok(())
    }

    fn finish(&self) -> Result<Value, SQLError> {
        Ok(Value::Str(self.parts.join(",")))
    }
}

#[test]
fn registered_aggregate_function_receives_ordered_inputs() {
    let eng = Engine::new();
    eng.register_aggregate_function("rust_join_observed", JoinObserved::default)
        .unwrap();
    eng.sql("CREATE TABLE samples (name TEXT, rank INTEGER)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO samples (name, rank) VALUES \
         ('first', 1), ('third', 3), ('second', 2)",
        &[],
    )
    .unwrap();

    let res = eng
        .sql(
            "SELECT rust_join_observed(name ORDER BY rank DESC) AS names FROM samples",
            &[],
        )
        .unwrap();

    assert_eq!(
        res.rows[0]["names"],
        Value::Str("third,second,first".into())
    );
}

#[derive(Default)]
struct AssertDescending {
    last: Option<i64>,
    count: i64,
}

impl SQLAggregateState for AssertDescending {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        let [Value::Int(value)] = args else {
            return Err(SQLError::BadArity {
                name: "rust_assert_desc".into(),
                expected: "1 integer argument".into(),
                actual: args.len(),
            });
        };
        if let Some(last) = self.last {
            if *value > last {
                return Err(SQLError::Internal(format!(
                    "registered aggregate observed {value} after {last}"
                )));
            }
        }
        self.last = Some(*value);
        self.count += 1;
        Ok(())
    }

    fn finish(&self) -> Result<Value, SQLError> {
        Ok(Value::Int(self.count))
    }
}

#[test]
fn registered_aggregate_order_by_spills_sorted_runs() {
    let eng = Engine::new();
    eng.register_aggregate_function("rust_assert_desc", AssertDescending::default)
        .unwrap();
    eng.sql("CREATE TABLE samples (n INTEGER)", &[]).unwrap();
    let values = (0..5000)
        .map(|n| format!("({n})"))
        .collect::<Vec<_>>()
        .join(", ");
    eng.sql(&format!("INSERT INTO samples (n) VALUES {values}"), &[])
        .unwrap();

    let res = eng
        .sql(
            "SELECT rust_assert_desc(n ORDER BY n DESC) AS count FROM samples",
            &[],
        )
        .unwrap();

    assert_eq!(res.rows[0]["count"], Value::Int(5000));
}
