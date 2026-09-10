//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned physical query results and public result conversion.

use crate::physical::physical_exec_error;
use uqa_core::Value;
use uqa_sql::{ResultRow, SQLError, SQLResult};

mod exists;
pub use exists::collect_exists_key_operator;

pub enum QueryRows {
    Rows {
        named: Vec<ResultRow>,
        positional: Option<Vec<Vec<Value>>>,
    },
    SharedSpill(crate::SharedSpill),
    ExistsKeySet(crate::CanonicalRowHashSet),
}

pub struct QueryOutput {
    pub columns: Vec<String>,
    pub column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    /// Physical columns include internal row metadata that is available to a parent query block but never exposed through [`SQLResult`].
    pub internal_columns: Vec<String>,
    pub internal_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    pub rows: QueryRows,
}

impl QueryOutput {
    pub fn into_cursor(self) -> Result<crate::query::cursor::SQLCursor, SQLError> {
        match self.rows {
            QueryRows::SharedSpill(rows) => {
                crate::query::cursor::SQLCursor::from_spill(self.columns, self.column_types, rows)
            }
            QueryRows::Rows { .. } | QueryRows::ExistsKeySet(_) => Err(SQLError::Internal(
                "cursor query unexpectedly used unbounded row materialization".into(),
            )),
        }
    }

    pub fn into_sql_result(self) -> Result<SQLResult, SQLError> {
        let (rows, positional_rows) = match self.rows {
            QueryRows::Rows { named, positional } => (named, positional),
            QueryRows::SharedSpill(rows) => {
                let mut scan = crate::SharedSpillScan::new(rows);
                (
                    crate::physical::run_to_rows(&mut scan)
                        .map_err(physical_exec_error)?
                        .1,
                    None,
                )
            }
            QueryRows::ExistsKeySet(_) => {
                return Err(SQLError::Internal(
                    "EXISTS key-set output cannot become a SQL result".into(),
                ));
            }
        };
        Ok(SQLResult::from_typed_rows_with_positions(
            self.columns,
            self.column_types,
            rows,
            positional_rows,
        ))
    }

    pub fn into_operator<'a>(self) -> Box<dyn crate::PhysicalOperator + 'a> {
        match self.rows {
            QueryRows::Rows { named, .. } => Box::new(crate::TableScan::from_typed_rows(
                self.internal_columns,
                self.internal_types,
                named,
            )),
            QueryRows::SharedSpill(rows) => Box::new(crate::SharedSpillScan::new(rows)),
            QueryRows::ExistsKeySet(_) => {
                panic!("EXISTS key-set output cannot become a physical operator")
            }
        }
    }

    pub fn into_public_operator<'a>(self) -> Box<dyn crate::PhysicalOperator + 'a> {
        let columns = self.columns.clone();
        let public_width = columns.len();
        let operator = self.into_operator();
        let positions = columns
            .into_iter()
            .enumerate()
            .map(|(position, column)| (column, position))
            .collect();
        debug_assert!(operator.row_schema().len() >= public_width);
        Box::new(crate::ColumnSelection::with_positions(operator, positions))
    }

    pub fn into_subquery_result(self) -> Result<crate::SubqueryResult, SQLError> {
        let QueryOutput {
            columns,
            column_types: _,
            internal_columns,
            internal_types,
            rows,
        } = self;
        let rows: Box<dyn Iterator<Item = Result<crate::OwnedPhysicalRow, SQLError>> + Send> =
            match rows {
                QueryRows::Rows { named, .. } => {
                    let schema = crate::RowSchema::with_types(internal_columns, internal_types);
                    Box::new(named.into_iter().map(move |row| {
                        Ok(crate::OwnedPhysicalRow::new(
                            schema.clone(),
                            crate::PhysicalRow::from_result_row(&schema, row),
                        ))
                    }))
                }
                QueryRows::SharedSpill(rows) => Box::new(
                    rows.read_rows()
                        .map_err(physical_exec_error)?
                        .map(|row| row.map_err(physical_exec_error)),
                ),
                QueryRows::ExistsKeySet(_) => {
                    return Err(SQLError::Internal(
                        "EXISTS key-set output cannot become a scalar subquery result".into(),
                    ));
                }
            };
        Ok(crate::SubqueryResult { columns, rows })
    }
}

pub fn query_output_shared(
    output: QueryOutput,
    label: &str,
) -> Result<crate::SharedSpill, SQLError> {
    let QueryRows::SharedSpill(rows) = output.rows else {
        return Err(SQLError::Internal(format!(
            "{label} execution returned in-memory rows at an internal streaming boundary"
        )));
    };
    Ok(rows)
}
