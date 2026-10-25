//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assignment conversion and outer projection run while the rule action emits its row.

use super::{
    coerce_image, source_index, RuleReturningImages, RuleReturningProjection, RuleReturningResult,
};
use crate::{
    mutation::{returning::ReturningRows, rules::RuleRows},
    query::consumer::{QueryConsumerControl, QueryRowConsumer},
    OwnedPhysicalRow, RowSchema,
};
use std::cell::RefCell;
use uqa_sql::{assignment::AssignmentContext, ast::ColumnDef, SQLError};

pub(crate) struct RuleReturningConsumer<'a> {
    assignment: &'a dyn AssignmentContext,
    columns: &'a [ColumnDef],
    source_rows: Option<RuleRows>,
    projection: Option<&'a dyn RuleReturningProjection>,
    rows: RefCell<ReturningRows>,
    schema: RefCell<Option<RowSchema>>,
}

impl<'a> RuleReturningConsumer<'a> {
    pub fn new(
        assignment: &'a dyn AssignmentContext,
        columns: &'a [ColumnDef],
        source_rows: Option<RuleRows>,
        projection: Option<&'a dyn RuleReturningProjection>,
        rows: ReturningRows,
    ) -> Self {
        Self {
            assignment,
            columns,
            source_rows,
            projection,
            rows: RefCell::new(rows),
            schema: RefCell::new(None),
        }
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
                source_rows: self.source_rows,
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
            .and_then(|width| width.checked_add(usize::from(self.source_rows.is_some())))
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
            self.source_rows.as_ref(),
            schema,
            &input,
        )?;
        if let Some(projection) = self.projection {
            let values = image.row.into_physical_values();
            let source = source_index(&values, self.columns.len() * 3, self.source_rows.as_ref())?
                .map(|index| {
                    self.source_rows
                        .as_ref()
                        .expect("validated source owner")
                        .get(index)
                })
                .transpose()?;
            let row = projection.project(
                &values,
                self.columns.len(),
                source.as_ref().and_then(|row| row.context.as_ref()),
            )?;
            self.rows
                .borrow_mut()
                .push_to(row, projection.destination())?;
        } else {
            self.rows.borrow_mut().push(image)?;
        }
        Ok(QueryConsumerControl::Continue)
    }
}
