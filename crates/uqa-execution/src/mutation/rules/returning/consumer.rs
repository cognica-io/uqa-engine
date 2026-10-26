//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assignment conversion and outer projection run while the rule action emits its row.

use super::{
    coerce_image, source_context, RuleReturningImages, RuleReturningProjection, RuleReturningResult,
};
use crate::{
    mutation::returning::ReturningRows,
    query::consumer::{QueryConsumerControl, QueryRowConsumer},
    OwnedPhysicalRow, RowSchema,
};
use std::cell::RefCell;
use uqa_sql::{assignment::AssignmentContext, ast::ColumnDef, SQLError};

pub(crate) struct RuleReturningConsumer<'a> {
    assignment: &'a dyn AssignmentContext,
    columns: &'a [ColumnDef],
    context_schema: Option<RowSchema>,
    projection: Option<&'a dyn RuleReturningProjection>,
    rows: RefCell<ReturningRows>,
    schema: RefCell<Option<RowSchema>>,
}

impl<'a> RuleReturningConsumer<'a> {
    pub fn new(
        assignment: &'a dyn AssignmentContext,
        columns: &'a [ColumnDef],
        projection: Option<&'a dyn RuleReturningProjection>,
        rows: ReturningRows,
    ) -> Self {
        Self {
            assignment,
            columns,
            context_schema: None,
            projection,
            rows: RefCell::new(rows),
            schema: RefCell::new(None),
        }
    }

    pub fn with_context(mut self, schema: Option<RowSchema>) -> Self {
        self.context_schema = schema;
        self
    }

    pub fn finish(self, affected_rows: u64) -> RuleReturningResult {
        let rows = self.rows.into_inner();
        if self.projection.is_some() {
            RuleReturningResult::Projected {
                rows,
                affected_rows,
            }
        } else {
            RuleReturningResult::Images(RuleReturningImages {
                images: rows,
                width: self.columns.len(),
                context_schema: self.context_schema,
                affected_rows,
            })
        }
    }
}

impl QueryRowConsumer for RuleReturningConsumer<'_> {
    fn begin(&self, _columns: &[String], schema: &RowSchema) -> Result<(), SQLError> {
        let expected = self
            .columns
            .len()
            .checked_mul(3)
            .and_then(|width| {
                width.checked_add(
                    self.context_schema
                        .as_ref()
                        .map_or(0, RowSchema::physical_width),
                )
            })
            .ok_or_else(|| SQLError::Internal("rewrite-rule RETURNING width overflow".into()))?;
        if schema.len() != expected {
            return Err(SQLError::Internal(format!(
                "rewrite-rule RETURNING provider produced {} columns, expected {expected}",
                schema.len()
            )));
        }
        *self.schema.borrow_mut() = Some(RowSchema::new(schema.columns().to_vec()));
        Ok(())
    }

    fn consume(&self, input: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        let schema = self.schema.borrow();
        let schema = schema.as_ref().ok_or_else(|| {
            SQLError::Internal("rule action emitted a row before its descriptor".into())
        })?;
        let image = coerce_image(
            self.assignment,
            self.columns,
            self.context_schema.as_ref(),
            schema,
            &input,
        )?;
        if let Some(projection) = self.projection {
            let values = image.row.into_physical_values();
            let source = source_context(
                &values,
                self.columns.len() * 3,
                self.context_schema.as_ref(),
            )?;
            let row = projection.project(&values, self.columns.len(), source.as_ref())?;
            self.rows
                .borrow_mut()
                .push_to(row, projection.destination())?;
        } else {
            self.rows.borrow_mut().push(image)?;
        }
        Ok(QueryConsumerControl::Continue)
    }
}
