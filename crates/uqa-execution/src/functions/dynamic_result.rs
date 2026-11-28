//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Conversion of dynamically shaped host callback results into named SQL rows.

use super::SQLTableFunctionResult;
use uqa_core::Value;

impl SQLTableFunctionResult {
    /// Accept an object with columns and rows, a column/row pair, or a list of named rows.
    pub fn from_dynamic_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Map(mut result) => {
                let columns = result
                    .remove("columns")
                    .ok_or_else(|| "table result object needs `columns`".to_string())?;
                let rows = result
                    .remove("rows")
                    .ok_or_else(|| "table result object needs `rows`".to_string())?;
                table_result_from_parts(columns, rows)
            }
            Value::List(mut pair) if is_table_result_pair(&pair) => {
                let rows = pair.pop().expect("table result pair has two values");
                let columns = pair.pop().expect("table result pair has two values");
                table_result_from_parts(columns, rows)
            }
            rows @ Value::List(_) => table_rows(rows, Vec::new()),
            _ => Err(
                "table callback must return { columns, rows }, [columns, rows], or row objects"
                    .to_string(),
            ),
        }
    }
}

fn is_table_result_pair(values: &[Value]) -> bool {
    values.len() == 2
        && matches!(&values[0], Value::List(columns) if columns.iter().all(|value| matches!(value, Value::Str(_) | Value::FixedChar(_))))
        && matches!(&values[1], Value::List(_))
}

fn table_result_from_parts(columns: Value, rows: Value) -> Result<SQLTableFunctionResult, String> {
    let Value::List(columns) = columns else {
        return Err("table result `columns` must be an array of strings".to_string());
    };
    let columns = columns
        .into_iter()
        .map(|column| match column {
            Value::Str(column) | Value::FixedChar(column) => Ok(column),
            _ => Err("table result `columns` must contain only strings".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    table_rows(rows, columns)
}

fn table_rows(rows: Value, mut columns: Vec<String>) -> Result<SQLTableFunctionResult, String> {
    let Value::List(rows) = rows else {
        return Err("table result `rows` must be an array".to_string());
    };
    let mut converted = Vec::with_capacity(rows.len());
    for row in rows {
        match row {
            Value::Map(values) => {
                if columns.is_empty() {
                    let (names, row) = values.into_iter().unzip();
                    columns = names;
                    converted.push(row);
                } else if values.keys().eq(columns.iter()) {
                    converted.push(values.into_values().collect());
                } else {
                    converted.push(
                        columns
                            .iter()
                            .map(|column| values.get(column).cloned().unwrap_or(Value::Null))
                            .collect(),
                    );
                }
            }
            Value::List(values) => {
                if columns.is_empty() {
                    return Err("table callback row arrays require explicit `columns`".to_string());
                }
                if values.len() != columns.len() {
                    return Err(format!(
                        "table callback row has {} values but {} columns",
                        values.len(),
                        columns.len()
                    ));
                }
                converted.push(values);
            }
            _ => return Err("each table callback row must be an object or array".to_string()),
        }
    }
    Ok(SQLTableFunctionResult {
        columns,
        rows: converted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_results_accept_objects_and_validate_row_width() {
        let result = SQLTableFunctionResult::from_dynamic_value(Value::List(vec![Value::Map(
            [("value".to_string(), Value::Int(3))].into_iter().collect(),
        )]))
        .unwrap();
        assert_eq!(result.columns, vec!["value"]);
        assert_eq!(result.rows, vec![vec![Value::Int(3)]]);

        let invalid = SQLTableFunctionResult::from_dynamic_value(Value::List(vec![
            Value::List(vec![Value::Str("a".to_string())]),
            Value::List(vec![Value::List(vec![Value::Int(1), Value::Int(2)])]),
        ]));
        assert!(invalid.is_err());
    }

    #[test]
    fn named_rows_follow_declared_column_order_and_repeat_values() {
        let labels = ["z", "a", "absent", "z"].map(|s| Value::Str(s.to_owned()));
        let row = Value::Map(
            [
                ("a".to_owned(), Value::Int(1)),
                ("z".to_owned(), Value::Int(2)),
            ]
            .into(),
        );
        let result = SQLTableFunctionResult::from_dynamic_value(Value::Map(
            [
                ("columns".to_owned(), Value::List(labels.to_vec())),
                ("rows".to_owned(), Value::List(vec![row])),
            ]
            .into(),
        ))
        .unwrap();
        assert_eq!(result.columns, ["z", "a", "absent", "z"]);
        assert_eq!(
            result.rows,
            [vec![
                Value::Int(2),
                Value::Int(1),
                Value::Null,
                Value::Int(2)
            ]]
        );
    }
}
