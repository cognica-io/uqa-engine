//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_engine::{
    Engine, SQLAggregateState, SQLScalarFunction, SQLTableFunction, SQLTableFunctionResult,
};
use uqa_sql::SQLError;

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
