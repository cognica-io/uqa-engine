//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table-function call metadata and positional result transport.

use crate::functions::{SQLTableFunctionResult, SQLTableFunctionStream};
use crate::ScalarExpr;
use uqa_core::Value;
use uqa_sql::SQLError;

#[derive(Clone, Copy)]
pub struct TableFunctionCall<'a> {
    pub name: &'a str,
    pub binding: Option<&'a uqa_sql::ast::FunctionBinding>,
    pub output_name: &'a str,
    pub relations: Option<&'a uqa_sql::ast::OperatorJoinRelations>,
    pub args: &'a [ScalarExpr],
    pub alias: Option<&'a str>,
    pub column_aliases: &'a [String],
    pub ordinality: bool,
    pub column_types: &'a [String],
}

/// SQL-visible column metadata paired with positional table-function rows. Column names never participate in row transport, so duplicate and unnamed outputs remain distinct physical attributes.
pub struct TableFunctionRows {
    pub columns: Vec<String>,
    pub rows: crate::PhysicalProjectRows,
}

impl TableFunctionRows {
    pub fn new(columns: Vec<String>, rows: crate::PhysicalProjectRows) -> Self {
        Self { columns, rows }
    }

    pub fn materialized(columns: Vec<String>, rows: Vec<Vec<Value>>) -> Self {
        Self::new(
            columns,
            Box::new(
                rows.into_iter()
                    .map(|values| Ok(crate::PhysicalRow::from_values(values))),
            ),
        )
    }
}

pub fn registered_table_function_rows(
    name: &str,
    result: SQLTableFunctionResult,
    _alias: Option<&str>,
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    if result.columns.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "table function `{name}` returned no columns"
        )));
    }
    let columns: Vec<String> = result
        .columns
        .iter()
        .enumerate()
        .map(|(idx, column)| {
            column_aliases
                .get(idx)
                .cloned()
                .unwrap_or_else(|| column.clone())
        })
        .collect();
    let mut out = Vec::with_capacity(result.rows.len());
    for values in result.rows {
        if values.len() != result.columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "table function `{name}` row has {} values for {} columns",
                values.len(),
                result.columns.len()
            )));
        }
        out.push(values);
    }
    Ok(TableFunctionRows::materialized(columns, out))
}

pub fn registered_table_function_row_stream(
    name: &str,
    result: SQLTableFunctionStream,
    _alias: Option<&str>,
    column_aliases: &[String],
) -> Result<TableFunctionRows, SQLError> {
    if result.columns.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "table function `{name}` returned no columns"
        )));
    }
    let expected_width = result.columns.len();
    let columns = result
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            column_aliases
                .get(index)
                .cloned()
                .unwrap_or_else(|| column.clone())
        })
        .collect::<Vec<_>>();
    let function_name = name.to_string();
    Ok(TableFunctionRows::new(
        columns,
        Box::new(
            result
                .rows
                .map(move |values| -> crate::ExecResult<crate::PhysicalRow> {
                    let values = values.map_err(crate::ExecError::from)?;
                    if values.len() != expected_width {
                        return Err(SQLError::TypeMismatch(format!(
                "table function `{function_name}` row has {} values for {expected_width} columns",
                values.len()
            ))
                        .into());
                    }
                    Ok(crate::PhysicalRow::from_values(values))
                }),
        ),
    ))
}

pub mod values;

pub mod rows_from;
