//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve consumed values against their lexical query and preceding-source scopes.

mod substitution;
#[cfg(test)]
mod tests;
pub use substitution::substitute_outer_columns;

use crate::plan::{AnalyzedSource, ProjectionPlan, QueryBlockPlan, QueryPlan, SourcePlan};
use crate::{RowSchema, SQLError, ScalarExpr};
use uqa_core::memory::ProductionControl;

mod traversal;
pub use traversal::ControlledScopedReferences;

/// Visit input references not provided by a nested query or preceding LATERAL input. Analysis must have retained every required source layout.
pub struct ScopedReferences<'a> {
    visitor: &'a mut dyn FnMut(&ScalarExpr),
    complete: bool,
}

impl<'a> ScopedReferences<'a> {
    pub fn new(visitor: &'a mut dyn FnMut(&ScalarExpr)) -> Self {
        Self {
            visitor,
            complete: true,
        }
    }

    pub fn expression(&mut self, expression: &ScalarExpr, subqueries: &[QueryPlan]) {
        self.observe(|references| references.expression(expression, subqueries));
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn query(&mut self, query: &QueryPlan) {
        self.observe(|references| references.query(query));
    }

    pub fn source(
        &mut self,
        source: &SourcePlan,
        analysis: Option<&AnalyzedSource>,
        subqueries: &[QueryPlan],
        preceding: &[RowSchema],
        inherited_lateral: bool,
    ) {
        self.observe(|references| {
            references.source(source, analysis, subqueries, preceding, inherited_lateral)
        });
    }

    fn observe(
        &mut self,
        read: impl FnOnce(&mut ControlledScopedReferences<'_, '_>) -> Result<(), SQLError>,
    ) {
        let mut visitor = |part: &ScalarExpr| {
            (self.visitor)(part);
            Ok(())
        };
        let mut references =
            ControlledScopedReferences::new(&mut visitor, ProductionControl::uncontrolled());
        read(&mut references)
            .expect("ordinary reference observation cannot be limited or cancelled");
        self.complete &= references.is_complete();
    }
}

/// SQL92 ORDER BY and DISTINCT ON names and positions observe an output already included in projection demand; expressions inside those clauses still resolve against the input namespace.
pub fn query_inputs(block: &QueryBlockPlan) -> impl Iterator<Item = &ScalarExpr> {
    block.query_expressions().filter(|expression| {
        !is_ordering(block, expression) || !ordering_uses_output(expression, &block.projections)
    })
}

/// Visit the same query inputs with admission for decoded output labels and cancellation before each observed root.
pub fn visit_query_inputs_with_control(
    block: &QueryBlockPlan,
    control: &ProductionControl<'_>,
    visitor: &mut impl FnMut(&ScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    control.check()?;
    for expression in block.query_expressions() {
        control.check()?;
        if !is_ordering(block, expression)
            || !ordering_uses_output_with_control(expression, &block.projections, control)?
        {
            visitor(expression)?;
        }
    }
    control.check()?;
    Ok(())
}

fn is_ordering(block: &QueryBlockPlan, expression: &ScalarExpr) -> bool {
    block
        .order_by
        .iter()
        .map(|order| &order.expr)
        .chain(&block.distinct_on)
        .any(|order| std::ptr::eq(order, expression))
}

/// SQL92 output names and ordinals bind to the selected output, while compound ordering expressions use the input namespace.
pub fn ordering_uses_output(expression: &ScalarExpr, projections: &[ProjectionPlan]) -> bool {
    ordering_uses_output_with_control(expression, projections, &ProductionControl::uncontrolled())
        .expect("ordinary output-label observation cannot be limited or cancelled")
}

fn ordering_uses_output_with_control(
    expression: &ScalarExpr,
    projections: &[ProjectionPlan],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    match expression {
        ScalarExpr::Position(_) | ScalarExpr::Literal(uqa_core::Value::Int(_)) => Ok(true),
        ScalarExpr::Column(name) => {
            for projection in projections {
                if crate::semantics::with_projection_label(projection, control, |label| {
                    Ok(label == name.as_str())
                })? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}
