//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pull-based scalar-subquery protocol and result consumers.

use uqa_core::Value;
use uqa_sql::expr::RowLookup;
use uqa_sql::{ResultRow, SQLError, SQLParam};

use crate::batch::{OwnedPhysicalRow, PhysicalRow, RowSchema};

use super::SubqueryId;

/// Runtime callback for query children referenced by [`SubqueryId`]. The planner owns the actual query-plan arena; execution only needs this stable slot interface.
pub trait ScalarSubqueryRunner {
    fn execute_subquery(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError>;

    fn execute_subquery_physical(
        &self,
        subquery: SubqueryId,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError> {
        let outer = outer_schema.view(outer_row);
        self.execute_subquery(subquery, Some(&outer), params)
    }

    fn scalar_subquery_value(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        self.execute_subquery(subquery, outer_row, params)?
            .into_scalar_value()
    }

    fn scalar_subquery_value_physical(
        &self,
        subquery: SubqueryId,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        self.execute_subquery_physical(subquery, outer_schema, outer_row, params)?
            .into_scalar_value()
    }

    fn subquery_exists(
        &self,
        subquery: SubqueryId,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        self.execute_subquery(subquery, outer_row, params)?
            .into_exists()
    }

    fn subquery_exists_physical(
        &self,
        subquery: SubqueryId,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        self.execute_subquery_physical(subquery, outer_schema, outer_row, params)?
            .into_exists()
    }

    fn subquery_contains(
        &self,
        subquery: SubqueryId,
        needle: &Value,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        self.execute_subquery(subquery, outer_row, params)?
            .contains(needle)
    }

    fn subquery_contains_physical(
        &self,
        subquery: SubqueryId,
        needle: &Value,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        self.execute_subquery_physical(subquery, outer_schema, outer_row, params)?
            .contains(needle)
    }
}

/// Pull-based scalar-subquery result. Scalar, EXISTS, and IN consumers never need to materialize the complete child relation: they respectively inspect at most two rows, one row, or one row at a time.
pub struct SubqueryResult {
    pub columns: Vec<String>,
    pub rows: Box<dyn Iterator<Item = Result<OwnedPhysicalRow, SQLError>> + Send>,
}

impl SubqueryResult {
    pub fn from_rows(columns: Vec<String>, rows: Vec<ResultRow>) -> Self {
        let schema = RowSchema::new(columns.clone());
        Self {
            columns,
            rows: Box::new(rows.into_iter().map(move |row| {
                Ok(OwnedPhysicalRow::new(
                    schema.clone(),
                    PhysicalRow::from_result_row(&schema, row),
                ))
            })),
        }
    }

    pub fn into_scalar_value(mut self) -> Result<Value, SQLError> {
        let Some(first_row) = self.rows.next().transpose()? else {
            return Ok(Value::Null);
        };
        if self.rows.next().transpose()?.is_some() {
            return Err(SQLError::TypeMismatch(
                "scalar subquery returned more than one row".into(),
            ));
        }
        if self.columns.is_empty() {
            return Err(SQLError::TypeMismatch(
                "scalar subquery returned no columns".into(),
            ));
        }
        Ok(first_row
            .positional_column(0)
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub fn into_exists(mut self) -> Result<bool, SQLError> {
        Ok(self.rows.next().transpose()?.is_some())
    }

    pub fn contains(self, needle: &Value) -> Result<Option<bool>, SQLError> {
        if self.columns.is_empty() {
            return Ok(Some(false));
        }
        let mut saw_row = false;
        let mut saw_null = false;
        for row in self.rows {
            let row = row?;
            saw_row = true;
            match row.positional_column(0) {
                Some(Value::Null) | None => saw_null = true,
                Some(value) if !matches!(needle, Value::Null) && value == needle => {
                    return Ok(Some(true));
                }
                Some(_) => {}
            }
        }
        Ok(if !saw_row {
            Some(false)
        } else if matches!(needle, Value::Null) || saw_null {
            None
        } else {
            Some(false)
        })
    }
}
