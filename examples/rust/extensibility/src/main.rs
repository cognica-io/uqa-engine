//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Run the extensibility scenario through the Rust engine API.

use uqa_core::Value;
use uqa_engine::{
    Engine, SQLAggregateState, SQLFunctionOptions, SQLFunctionVolatility, SQLResult,
    SQLTableFunctionResult,
};
use uqa_sql::SQLError;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();
    load_samples(&engine)?;
    let options = SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable);
    engine.register_scalar_function_with_options("normalize_label", options, normalize_label)?;
    engine.register_table_function_with_options("repeat_rows", options, repeat_rows)?;
    engine.register_aggregate_function_with_options("sum_squares", options, SumSquares::default)?;

    let results = run_queries(&engine)?;
    verify_results(&results);
    println!(
        "Rust extensibility example passed: {:?}",
        results.scalar.rows
    );
    engine.close()?;
    Ok(())
}

fn load_samples(engine: &Engine) -> Result<(), SQLError> {
    engine.sql(
        "CREATE TABLE samples (grp TEXT, label TEXT, value INTEGER)",
        &[],
    )?;
    engine.sql(
        "INSERT INTO samples (grp, label, value) VALUES \
         ('a', ' SQL Manual ', 1), ('a', 'Node JS', 2), ('b', 'Browser WASM', 3)",
        &[],
    )?;
    Ok(())
}

fn normalize_label(args: &[Value]) -> Result<Value, SQLError> {
    let [value] = args else {
        return Err(SQLError::BadArity {
            name: "normalize_label".to_string(),
            expected: "1 text argument".to_string(),
            actual: args.len(),
        });
    };
    match value {
        Value::Str(value) | Value::FixedChar(value) => {
            Ok(Value::Str(value.trim().to_lowercase().replace(' ', "-")))
        }
        other => Err(SQLError::TypeMismatch(format!(
            "normalize_label expects text, got {other:?}"
        ))),
    }
}

fn repeat_rows(args: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
    let [label, times] = args else {
        return Err(SQLError::BadArity {
            name: "repeat_rows".to_string(),
            expected: "text and non-negative integer".to_string(),
            actual: args.len(),
        });
    };
    let (Value::Str(label) | Value::FixedChar(label)) = label else {
        return Err(SQLError::TypeMismatch(
            "repeat_rows expects text as its first argument".to_string(),
        ));
    };
    let Value::Int(times) = times else {
        return Err(SQLError::TypeMismatch(
            "repeat_rows expects an integer as its second argument".to_string(),
        ));
    };
    let count = usize::try_from(*times).map_err(|_| {
        SQLError::TypeMismatch("repeat_rows expects a non-negative row count".to_string())
    })?;
    let rows = (0..count)
        .map(|index| vec![Value::Str(label.clone()), Value::Int(index as i64)])
        .collect();
    Ok(SQLTableFunctionResult::new(["label", "idx"], rows))
}

#[derive(Default)]
struct SumSquares {
    total: i64,
}

impl SQLAggregateState for SumSquares {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        let [value] = args else {
            return Err(SQLError::BadArity {
                name: "sum_squares".to_string(),
                expected: "1 integer argument".to_string(),
                actual: args.len(),
            });
        };
        match value {
            Value::Null => Ok(()),
            Value::Int(value) => {
                let square = value.checked_mul(*value).ok_or_else(|| {
                    SQLError::Internal("sum_squares multiplication overflow".to_string())
                })?;
                self.total = self.total.checked_add(square).ok_or_else(|| {
                    SQLError::Internal("sum_squares addition overflow".to_string())
                })?;
                Ok(())
            }
            other => Err(SQLError::TypeMismatch(format!(
                "sum_squares expects an integer, got {other:?}"
            ))),
        }
    }

    fn finish(&self) -> Result<Value, SQLError> {
        Ok(Value::Int(self.total))
    }
}

struct QueryResults {
    scalar: SQLResult,
    table: SQLResult,
    aggregate: SQLResult,
}

fn run_queries(engine: &Engine) -> Result<QueryResults, SQLError> {
    Ok(QueryResults {
        scalar: engine.sql(
            "SELECT normalize_label(label) AS label FROM samples ORDER BY value",
            &[],
        )?,
        table: engine.sql(
            "SELECT label, idx FROM repeat_rows('row', 3) AS r(label, idx) ORDER BY idx",
            &[],
        )?,
        aggregate: engine.sql(
            "SELECT grp, sum_squares(value) AS total FROM samples GROUP BY grp ORDER BY grp",
            &[],
        )?,
    })
}

fn verify_results(results: &QueryResults) {
    assert_eq!(
        column_values(&results.scalar, "label"),
        vec![
            Value::Str("sql-manual".to_string()),
            Value::Str("node-js".to_string()),
            Value::Str("browser-wasm".to_string()),
        ]
    );
    assert_eq!(
        column_values(&results.table, "label"),
        vec![
            Value::Str("row".to_string()),
            Value::Str("row".to_string()),
            Value::Str("row".to_string()),
        ]
    );
    assert_eq!(
        column_values(&results.table, "idx"),
        vec![Value::Int(0), Value::Int(1), Value::Int(2)]
    );
    assert_eq!(
        column_values(&results.aggregate, "grp"),
        vec![Value::Str("a".to_string()), Value::Str("b".to_string())]
    );
    assert_eq!(
        column_values(&results.aggregate, "total"),
        vec![Value::Int(5), Value::Int(9)]
    );
}

fn column_values(result: &SQLResult, column: &str) -> Vec<Value> {
    result
        .rows
        .iter()
        .map(|row| row.get(column).cloned().unwrap_or(Value::Null))
        .collect()
}
