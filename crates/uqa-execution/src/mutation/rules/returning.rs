//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical capture and outer projection of a rule action's RETURNING rows.
use crate::mutation::output::MutationOutput;
use crate::{
    mutation::returning::{
        build_returning_value_row, dml_returning_result, DmlReturningShape,
        ReturningExecutionContext, ReturningRows, ReturningValueProjectionRow,
    },
    OwnedPhysicalRow, PhysicalRow, RowSchema,
};
use uqa_core::Value;
use uqa_sql::SQLError;

mod consumer;
pub(super) use consumer::RuleReturningConsumer;
mod projection;
pub use projection::{RuleOutputProjection, RuleReturningProjection};

pub enum RuleReturningResult {
    Images(RuleReturningImages),
    Projected {
        rows: ReturningRows,
        affected_rows: u64,
    },
}

impl RuleReturningResult {
    pub fn empty() -> Self {
        Self::Images(RuleReturningImages::empty())
    }

    pub fn project<S: Clone + 'static>(
        self,
        context: ReturningExecutionContext<'_, S>,
        shape: DmlReturningShape<'_, S>,
    ) -> Result<MutationOutput, SQLError> {
        match self {
            Self::Images(images) => images.project(&context, shape),
            Self::Projected {
                rows,
                affected_rows,
            } => dml_returning_result(context, shape, rows, affected_rows),
        }
    }
}

pub struct RuleReturningImages {
    images: ReturningRows,
    width: usize,
    context_schema: Option<RowSchema>,
    affected_rows: u64,
}

impl RuleReturningImages {
    pub fn empty() -> Self {
        Self {
            images: ReturningRows::default(),
            width: 0,
            context_schema: None,
            affected_rows: 0,
        }
    }

    pub fn project<S: Clone + 'static>(
        self,
        context: &ReturningExecutionContext<'_, S>,
        shape: DmlReturningShape<'_, S>,
    ) -> Result<MutationOutput, SQLError> {
        let mut rows = ReturningRows::new(context.relational.runtime)?;
        for image in self.images.into_rows()? {
            let image = image?;
            let values = image.row.into_physical_values();
            let source = source_context(&values, self.width * 3, self.context_schema.as_ref())?;
            rows.push_to(
                build_returning_value_row(
                    *context,
                    ReturningValueProjectionRow {
                        table: shape.table,
                        target_qualifier: shape.target_qualifier,
                        current: &values[..self.width],
                        old: Some(&values[self.width..self.width * 2]),
                        new: Some(&values[self.width * 2..self.width * 3]),
                        aliases: shape.aliases,
                        context: source.as_ref(),
                    },
                    shape.returning,
                    shape.params,
                    shape.ctes,
                )?,
                context.destination,
            )?;
        }
        dml_returning_result(*context, shape, rows, self.affected_rows)
    }
}

fn coerce_image(
    assignment: &dyn uqa_sql::assignment::AssignmentContext,
    columns: &[uqa_sql::ast::ColumnDef],
    context_schema: Option<&RowSchema>,
    schema: &RowSchema,
    input: &OwnedPhysicalRow,
) -> Result<OwnedPhysicalRow, SQLError> {
    let width = columns.len();
    let image_width = width.saturating_mul(3);
    let expected_width = image_width + context_schema.map_or(0, RowSchema::physical_width);
    let row = input.view();
    let mut values = Vec::with_capacity(expected_width);
    for offset in [0, width, width * 2] {
        for (position, column) in columns.iter().enumerate() {
            let value = row.value_at(offset + position).cloned().ok_or_else(|| {
                SQLError::Internal("rewrite-rule RETURNING provider lost a positional value".into())
            })?;
            values.push(uqa_sql::assignment::conversion::coerce_assignment_value(
                assignment,
                value,
                &column.ty,
                input.schema.column_type(offset + position),
            )?);
        }
    }
    for position in image_width..expected_width {
        values.push(row.value_at(position).cloned().ok_or_else(|| {
            SQLError::Internal("rewrite-rule RETURNING provider lost its source context".into())
        })?);
    }
    Ok(OwnedPhysicalRow::new(
        schema.clone(),
        PhysicalRow::from_values(values),
    ))
}

fn source_context(
    values: &[Value],
    image_width: usize,
    schema: Option<&RowSchema>,
) -> Result<Option<OwnedPhysicalRow>, SQLError> {
    schema
        .map(|schema| {
            let context = values
                .get(image_width..)
                .filter(|values| values.len() == schema.physical_width())
                .ok_or_else(|| {
                    SQLError::Internal(
                        "rewrite-rule RETURNING provider lost its source context".into(),
                    )
                })?;
            Ok(OwnedPhysicalRow::new(
                schema.clone(),
                PhysicalRow::from_values(context.to_vec()),
            ))
        })
        .transpose()
}

#[cfg(test)]
mod tests;
