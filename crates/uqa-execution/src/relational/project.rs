//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar projection and star expansion.

use uqa_core::Value;

use super::{
    BackwardScanSupport, Batch, DefaultExpressionEvaluator, ExecResult, PhysicalOperator,
    PhysicalScanDirection, RowSchema, SQLParam, ScalarExpr, SharedExpressionEvaluator,
};
use crate::batch::ProjectedSlot;
use crate::spill::{SpillBatchWriter, SpillDrain};

pub use uqa_sql::plan::ProjectionTarget;

#[cfg(test)]
mod tests;

/// Per-row scalar projection. Each `(alias, expr)` pair is evaluated
/// against the input row and written under `alias` in the output. The
/// child schema is replaced with the output aliases.
pub struct Project<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    input_pending: crate::physical::demand::PendingRows,
    computed: crate::scalar::PreparedExpressions<Vec<ScalarExpr>>,
    evaluator: SharedExpressionEvaluator<'a>,
    schema: RowSchema,
    ordering: Vec<crate::PhysicalOrder>,
    work_mem: Option<usize>,
    pending: Option<SpillDrain>,
}

fn projection_layout(
    input: &RowSchema,
    projections: Vec<(ProjectionTarget, ScalarExpr)>,
    evaluator: &SharedExpressionEvaluator<'_>,
    pass_through: bool,
) -> (RowSchema, Vec<ScalarExpr>) {
    let mut projected = Vec::new();
    let mut projected_internal = Vec::new();
    let mut computed = Vec::new();
    for (target, expression) in projections {
        if let ScalarExpr::QualifiedStar(qualifier) = &expression {
            let ProjectionTarget::Column(_) = target else {
                unreachable!("an internal projection target cannot expand a qualified star");
            };
            if pass_through {
                continue;
            }
            for (column, logical, _, ty) in input.qualified_star_position_layout(qualifier) {
                if logical.is_some_and(|position| !evaluator.star_position_visible(input, position))
                {
                    continue;
                }
                let slot = logical
                    .and_then(|logical| input.physical_slot(logical))
                    .or_else(|| {
                        input.physical_slot_for_identity(&crate::ColumnIdentity::qualified(
                            qualifier, &column,
                        ))
                    });
                let collation = logical.map_or_else(
                    || input.qualified_collation(qualifier, &column),
                    |position| input.column_collation(position),
                );
                projected.push((
                    column,
                    ty,
                    ProjectedSlot::BoundInput {
                        physical: slot,
                        collation,
                    },
                ));
            }
            continue;
        }
        if matches!(expression, ScalarExpr::Star) {
            let ProjectionTarget::Column(_) = target else {
                unreachable!("an internal projection target cannot expand a star");
            };
            if pass_through {
                continue;
            }
            for (logical, column) in input.iter().enumerate() {
                if !evaluator.star_position_visible(input, logical) {
                    continue;
                }
                projected.push((
                    input.public_name(logical).unwrap_or(column).to_string(),
                    input.column_type(logical).cloned(),
                    ProjectedSlot::BoundInput {
                        physical: input.physical_slot(logical),
                        collation: input.column_collation(logical),
                    },
                ));
            }
            continue;
        }

        let ty = evaluator.expression_type(&expression, input).ok().flatten();
        let source = if let Some(source) = input.projection_input(&expression) {
            source
        } else {
            let position = computed.len();
            let collation = input.input_collation(&expression);
            computed.push(expression);
            ProjectedSlot::Computed {
                position,
                collation,
            }
        };
        match target {
            ProjectionTarget::Column(name) => projected.push((name, ty, source)),
            ProjectionTarget::Internal(column) => {
                projected_internal.push((column, ty, source));
            }
        }
    }
    let computed_count = computed.len();
    (
        RowSchema::project_with_sources(
            input,
            projected,
            projected_internal,
            computed_count,
            pass_through,
        ),
        computed,
    )
}

impl Project<'static> {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(String, ScalarExpr)>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::with_evaluator(
            child,
            projections,
            DefaultExpressionEvaluator::shared(params),
        )
    }

    pub fn with_targets(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(ProjectionTarget, ScalarExpr)>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::with_target_evaluator(
            child,
            projections,
            DefaultExpressionEvaluator::shared(params),
        )
    }

    /// Variant that keeps every input column in the output and appends
    /// the projections at the end. Used by aggregate / window paths.
    pub fn appending(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(String, ScalarExpr)>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::appending_with_evaluator(
            child,
            projections,
            DefaultExpressionEvaluator::shared(params),
        )
    }

    pub fn appending_targets(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(ProjectionTarget, ScalarExpr)>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::appending_target_evaluator(
            child,
            projections,
            DefaultExpressionEvaluator::shared(params),
        )
    }
}

impl<'a> Project<'a> {
    pub fn with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: Vec<(String, ScalarExpr)>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        Self::with_target_evaluator(
            child,
            projections
                .into_iter()
                .map(|(name, expression)| (ProjectionTarget::Column(name), expression))
                .collect(),
            evaluator,
        )
    }

    pub fn with_target_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: Vec<(ProjectionTarget, ScalarExpr)>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let projections = projections
            .into_iter()
            .map(|(target, expression)| {
                (
                    target,
                    evaluator.bind_type_introspection(expression, child.row_schema()),
                )
            })
            .collect::<Vec<_>>();
        let (schema, computed) =
            projection_layout(child.row_schema(), projections, &evaluator, false);
        Self {
            child,
            input_pending: crate::physical::demand::PendingRows::default(),
            computed: crate::scalar::PreparedExpressions::scalars(computed),
            evaluator,
            schema,
            ordering: Vec::new(),
            work_mem: None,
            pending: None,
        }
    }

    pub fn appending_with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: Vec<(String, ScalarExpr)>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        Self::appending_target_evaluator(
            child,
            projections
                .into_iter()
                .map(|(name, expression)| (ProjectionTarget::Column(name), expression))
                .collect(),
            evaluator,
        )
    }

    pub fn appending_target_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: Vec<(ProjectionTarget, ScalarExpr)>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let projections = projections
            .into_iter()
            .map(|(target, expression)| {
                (
                    target,
                    evaluator.bind_type_introspection(expression, child.row_schema()),
                )
            })
            .collect::<Vec<_>>();
        let ordering = child
            .output_ordering()
            .iter()
            .take_while(|order| {
                projections.iter().all(|(target, expression)| {
                    let ProjectionTarget::Column(name) = target else {
                        return true;
                    };
                    child
                        .row_schema()
                        .columns()
                        .iter()
                        .position(|column| column == name)
                        != Some(order.position)
                        || matches!(expression, ScalarExpr::Star | ScalarExpr::QualifiedStar(_))
                        || crate::order_expression_position(child.row_schema(), expression)
                            == Some(order.position)
                })
            })
            .cloned()
            .collect();
        let (schema, computed) =
            projection_layout(child.row_schema(), projections, &evaluator, true);
        Self {
            child,
            input_pending: crate::physical::demand::PendingRows::default(),
            computed: crate::scalar::PreparedExpressions::scalars(computed),
            evaluator,
            schema,
            ordering,
            work_mem: None,
            pending: None,
        }
    }

    /// Bound evaluated output while retaining the original input-batch evaluation boundary.
    pub fn with_memory_budget(mut self, bytes: usize) -> Self {
        self.work_mem = Some(bytes);
        self
    }

    fn prepare_batch(&mut self, batch: Batch) -> ExecResult<Option<Batch>> {
        let Some(budget) = self
            .work_mem
            .filter(|_| !self.computed.is_empty() && batch.rows.len() > 1)
        else {
            return self.project_batch(batch).map(Some);
        };
        // Finish every expression in this input batch before exposing its first result. Only the storage of those evaluated rows changes.
        let mut output = crate::SpillBuffer::new(budget / 2);
        let mut writer = SpillBatchWriter::new(self.schema.clone())?;
        for row in batch.rows {
            let values = self.project_values(&batch.schema, &row)?;
            writer.push(&mut output, row.append_values(values))?;
        }
        writer.finish(&mut output)?;
        self.pending = Some(output.drain()?);
        self.next_pending()
    }

    fn next_pending(&mut self) -> ExecResult<Option<Batch>> {
        if self.pending.is_some() {
            if let Err(error) = uqa_sql::expr::check_statement_cancellation() {
                self.pending = None;
                return Err(uqa_sql::SQLError::from(error).into());
            }
        }
        let batch = self.pending.as_mut().and_then(Iterator::next).transpose();
        if !matches!(batch, Ok(Some(_))) {
            self.pending = None;
        }
        batch
    }

    fn project_values(
        &self,
        schema: &RowSchema,
        row: &crate::PhysicalRow,
    ) -> ExecResult<Vec<Value>> {
        self.computed
            .iter()
            .map(|expression| {
                self.evaluator.evaluate_physical_with_function_states(
                    expression,
                    schema,
                    row,
                    self.computed.calls(),
                )
            })
            .collect()
    }

    fn project_batch(&self, batch: Batch) -> ExecResult<Batch> {
        if self.computed.is_empty() {
            return Ok(Batch::from_physical_rows(self.schema.clone(), batch.rows));
        }
        let mut out = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            let values = self.project_values(&batch.schema, &row)?;
            out.push(row.append_values(values));
        }
        Ok(Batch::from_physical_rows(self.schema.clone(), out))
    }
}

impl Project<'_> {
    fn pull(&mut self, demand: Option<std::num::NonZeroUsize>) -> ExecResult<Option<Batch>> {
        if let Some(batch) = self.next_pending()? {
            return Ok(Some(batch));
        }
        let Some(batch) = self.input_pending.pull(self.child.as_mut(), demand)? else {
            return Ok(None);
        };
        self.prepare_batch(batch)
    }
}

impl PhysicalOperator for Project<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        &self.ordering
    }

    fn backward_scan_support(&self) -> BackwardScanSupport {
        let child = self.child.backward_scan_support();
        if child == BackwardScanSupport::Native || self.computed.is_empty() {
            child
        } else {
            BackwardScanSupport::Unsupported
        }
    }

    fn open(&mut self) -> ExecResult<()> {
        self.input_pending.clear();
        self.pending = None;
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.pull(None)
    }

    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(Some(rows))
    }

    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        if let Some(batch) = self.next_pending()? {
            return Ok(Some(batch));
        }
        let Some(batch) = self.child.next_direction(direction)? else {
            return Ok(None);
        };
        self.prepare_batch(batch)
    }

    fn rewind(&mut self) -> ExecResult<()> {
        self.input_pending.clear();
        self.pending = None;
        self.child.rewind()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.input_pending.clear();
        self.pending = None;
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Sort
// -------------------------------------------------------------------------
