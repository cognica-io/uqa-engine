//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical capture and outer projection of a rule action's RETURNING rows.
use super::RuleRows;
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
    source_rows: Option<RuleRows>,
    affected_rows: u64,
}

impl RuleReturningImages {
    pub fn empty() -> Self {
        Self {
            images: ReturningRows::default(),
            width: 0,
            source_rows: None,
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
            let source = source_index(&values, self.width * 3, self.source_rows.as_ref())?
                .map(|index| {
                    self.source_rows
                        .as_ref()
                        .expect("validated source owner")
                        .get(index)
                })
                .transpose()?;
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
                        context: source.as_ref().and_then(|source| source.context.as_ref()),
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

#[cfg(test)]
fn capture_rule_returning_result(
    mut images: ReturningRows,
    assignment: &dyn uqa_sql::assignment::AssignmentContext,
    result: MutationOutput,
    columns: &[uqa_sql::ast::ColumnDef],
    source_rows: Option<RuleRows>,
) -> Result<RuleReturningImages, SQLError> {
    let width = columns.len();
    let image_width = width.saturating_mul(3);
    let expected_width = image_width + usize::from(source_rows.is_some());
    if result.schema.len() != expected_width {
        return Err(SQLError::Internal(format!(
            "rewrite-rule RETURNING provider produced {} columns, expected {}",
            result.schema.len(),
            expected_width
        )));
    }
    let schema = RowSchema::new(result.schema.columns().to_vec());
    let affected_rows = result.affected_rows;
    for batch in result.into_batches() {
        let batch = batch?;
        for row in batch.rows {
            let row = OwnedPhysicalRow::new(batch.schema.clone(), row);
            images.push(coerce_image(
                assignment,
                columns,
                source_rows.as_ref(),
                &schema,
                &row,
            )?)?;
        }
    }
    Ok(RuleReturningImages {
        images,
        width,
        source_rows,
        affected_rows,
    })
}

fn coerce_image(
    assignment: &dyn uqa_sql::assignment::AssignmentContext,
    columns: &[uqa_sql::ast::ColumnDef],
    source_rows: Option<&RuleRows>,
    schema: &RowSchema,
    input: &OwnedPhysicalRow,
) -> Result<OwnedPhysicalRow, SQLError> {
    let width = columns.len();
    let image_width = width.saturating_mul(3);
    let expected_width = image_width + usize::from(source_rows.is_some());
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
    if source_rows.is_some() {
        values.push(row.value_at(image_width).cloned().ok_or_else(|| {
            SQLError::Internal("rewrite-rule RETURNING provider lost its source index".into())
        })?);
    }
    source_index(&values, image_width, source_rows)?;
    Ok(OwnedPhysicalRow::new(
        schema.clone(),
        PhysicalRow::from_values(values),
    ))
}

fn source_index(
    values: &[Value],
    image_width: usize,
    source_rows: Option<&RuleRows>,
) -> Result<Option<usize>, SQLError> {
    let Some(source_rows) = source_rows else {
        return Ok(None);
    };
    let source_index = match values.get(image_width) {
        Some(Value::Int(index)) if *index >= 0 => usize::try_from(*index).map_err(|_| {
            SQLError::Internal("rewrite-rule RETURNING source index exceeds usize".into())
        })?,
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "rewrite-rule RETURNING source index is not a non-negative integer: {value:?}"
            )))
        }
        None => {
            return Err(SQLError::Internal(
                "rewrite-rule RETURNING provider lost its source index".into(),
            ))
        }
    };
    (source_index < source_rows.len())
        .then_some(Some(source_index))
        .ok_or_else(|| {
            SQLError::Internal(
                "rewrite-rule RETURNING source index is outside the event row set".into(),
            )
        })
}

#[cfg(test)]
mod tests;
