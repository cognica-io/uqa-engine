//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pull-based scalar-subquery protocol and result consumers.

use crate::RowSchemaExecution;
use uqa_core::Value;
use uqa_sql::expr::RowLookup;
use uqa_sql::{ResultRow, SQLError, SQLParam};

use crate::batch::{OwnedPhysicalRow, PhysicalRow, RowSchema};

use super::SubqueryId;

/// A SQL membership operand with lazy declared-type validation. The cache invokes `can_hash` once when selecting its representation, never once per probe or input row.
pub struct SubqueryNeedle<'a> {
    pub value: &'a Value,
    pub engine: Option<&'a dyn uqa_sql::expr::EngineHook>,
    pub control: uqa_core::memory::ProductionControl<'a>,
    pub can_hash: &'a mut dyn FnMut(Option<&uqa_sql::ColumnType>) -> Result<bool, SQLError>,
}

/// Runtime callback for query children referenced by [`SubqueryId`]. The planner owns the actual query-plan arena; execution only needs this stable slot interface.
pub trait ScalarSubqueryRunner {
    fn subquery_contains_selected(
        &self,
        subquery: SubqueryId,
        needle: SubqueryNeedle<'_>,
        outer_row: Option<&dyn RowLookup>,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        needle.control.check()?;
        self.execute_subquery(subquery, outer_row, params)?
            .contains_with_engine_and_control(needle.value, needle.engine, &needle.control)
    }

    fn subquery_contains_selected_physical(
        &self,
        subquery: SubqueryId,
        needle: SubqueryNeedle<'_>,
        outer_schema: &RowSchema,
        outer_row: &PhysicalRow,
        params: &[SQLParam],
    ) -> Result<Option<bool>, SQLError> {
        needle.control.check()?;
        self.execute_subquery_physical(subquery, outer_schema, outer_row, params)?
            .contains_with_engine_and_control(needle.value, needle.engine, &needle.control)
    }

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

    pub fn into_single_row(mut self) -> Result<Option<crate::OwnedPhysicalRow>, SQLError> {
        let Some(first) = self.rows.next().transpose()? else {
            return Ok(None);
        };
        if self.rows.next().transpose()?.is_some() {
            return Err(single_row_cardinality_error());
        }
        Ok(Some(first))
    }

    pub fn into_scalar_value(mut self) -> Result<Value, SQLError> {
        let Some(first_row) = self.rows.next().transpose()? else {
            return Ok(Value::Null);
        };
        if self.rows.next().transpose()?.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "21000".into(),
                message: "more than one row returned by a subquery used as an expression".into(),
            });
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
        self.contains_matching(
            needle,
            &uqa_core::memory::ProductionControl::uncontrolled(),
            |value, _| Ok(value == needle),
        )
    }

    /// Test a streamed SQL subquery with the same selected field operators as its cached membership path.
    pub fn contains_with_catalog(
        self,
        needle: &Value,
        catalog: &dyn uqa_sql::expr::SQLValueCatalog,
    ) -> Result<Option<bool>, SQLError> {
        self.contains_matching(
            needle,
            &uqa_core::memory::ProductionControl::uncontrolled(),
            |value, ty| {
                uqa_sql::expr::equal_selected_values_with_catalog(
                    value,
                    needle,
                    ty,
                    &uqa_core::memory::ProductionControl::uncontrolled(),
                    Some(catalog),
                )
            },
        )
    }

    pub fn contains_with_engine(
        self,
        needle: &Value,
        engine: Option<&dyn uqa_sql::expr::EngineHook>,
    ) -> Result<Option<bool>, SQLError> {
        self.contains_with_engine_and_control(
            needle,
            engine,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
    }

    pub fn contains_with_engine_and_control(
        self,
        needle: &Value,
        engine: Option<&dyn uqa_sql::expr::EngineHook>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<bool>, SQLError> {
        self.contains_matching(needle, control, |value, ty| {
            uqa_sql::expr::equal_selected_values_with_engine(value, needle, ty, control, engine)
        })
    }

    fn contains_matching(
        self,
        needle: &Value,
        control: &uqa_core::memory::ProductionControl<'_>,
        mut equal: impl FnMut(&Value, Option<&uqa_sql::ColumnType>) -> Result<bool, SQLError>,
    ) -> Result<Option<bool>, SQLError> {
        control.check()?;
        if self.columns.is_empty() {
            return Ok(Some(false));
        }
        let mut saw_row = false;
        let mut saw_null = false;
        for row in self.rows {
            control.check()?;
            let row = row?;
            saw_row = true;
            match row.positional_column(0) {
                Some(Value::Null) | None => saw_null = true,
                Some(value)
                    if !matches!(needle, Value::Null)
                        && equal(value, row.schema.column_type(0))? =>
                {
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

pub(crate) fn single_row_cardinality_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "21000".into(),
        message: "more than one row returned by a subquery used as an expression".into(),
    }
}
