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

/// Identity assigned to one projection result. SQL columns participate in
/// ordinary name binding and wildcard expansion; internal attributes are
/// executor-only `resjunk` slots addressed structurally.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectionTarget {
    Column(String),
    Internal(uqa_sql::ast::InternalColumnRef),
}

impl From<String> for ProjectionTarget {
    fn from(value: String) -> Self {
        Self::Column(value)
    }
}

impl From<&str> for ProjectionTarget {
    fn from(value: &str) -> Self {
        Self::Column(value.to_string())
    }
}

/// Per-row scalar projection. Each `(alias, expr)` pair is evaluated
/// against the input row and written under `alias` in the output. The
/// child schema is replaced with the output aliases.
pub struct Project<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    computed: Vec<ScalarExpr>,
    evaluator: SharedExpressionEvaluator<'a>,
    schema: RowSchema,
    ordering: Vec<crate::PhysicalOrder>,
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
                projected.push((column, ty, ProjectedSlot::Input(slot)));
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
                    ProjectedSlot::Input(input.physical_slot(logical)),
                ));
            }
            continue;
        }

        let ty = evaluator.expression_type(&expression, input).ok().flatten();
        let source = if let Some(logical) = crate::order_expression_position(input, &expression) {
            ProjectedSlot::Input(input.physical_slot(logical))
        } else {
            let position = computed.len();
            computed.push(expression);
            ProjectedSlot::Computed(position)
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
            computed,
            evaluator,
            schema,
            ordering: Vec::new(),
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
            computed,
            evaluator,
            schema,
            ordering,
        }
    }

    fn project_batch(&self, batch: Batch) -> ExecResult<Batch> {
        if self.computed.is_empty() {
            return Ok(Batch::from_physical_rows(self.schema.clone(), batch.rows));
        }
        let mut out = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            let values = self
                .computed
                .iter()
                .map(|expression| {
                    self.evaluator
                        .evaluate_physical(expression, &batch.schema, &row)
                })
                .collect::<ExecResult<Vec<Value>>>()?;
            out.push(row.append_values(values));
        }
        Ok(Batch::from_physical_rows(self.schema.clone(), out))
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
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next()? else {
            return Ok(None);
        };
        self.project_batch(batch).map(Some)
    }

    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next_direction(direction)? else {
            return Ok(None);
        };
        self.project_batch(batch).map(Some)
    }

    fn rewind(&mut self) -> ExecResult<()> {
        self.child.rewind()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Sort
// -------------------------------------------------------------------------
