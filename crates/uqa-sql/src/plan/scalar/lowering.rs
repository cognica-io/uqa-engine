//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! AST lowering can publish independently movable scalar children directly.

use super::{
    resources::{Control, Lowering, Result},
    source::Source,
    AggregateClassifier, Expr, OwnedScalarExpr, QueryLowering, ScalarExpr, ScalarMemory,
};
use uqa_core::memory::{BudgetedVec, ProductionControl};

impl super::super::ExpressionPlan {
    /// Lower borrowed scalar syntax directly into a mutable owner. Every child retains its own allocation lease, so subsequent selection or replacement releases discarded payloads without copying the completed scalar tree. Query children retain the scalar-only lowerer's rejection behavior.
    pub fn lower_scalar_for_rewrite(
        expression: &Expr,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr> {
        lower_owned(
            Source::Borrowed(expression),
            &super::super::NoRegisteredAggregates,
            &mut Vec::new(),
            *control,
        )
    }
}

impl Lowering<'_> {
    pub(super) fn expression(
        &mut self,
        expression: Source<'_, Expr>,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut dyn QueryLowering,
    ) -> Result<ScalarExpr> {
        if self.owned_children.is_none() {
            return self.expression_node(expression, aggregates, subqueries);
        }
        let control = self
            .control
            .as_ref()
            .expect("mutable scalar lowering")
            .production;
        self.owned_children
            .as_mut()
            .expect("mutable scalar lowering")
            .reserve(1)?;
        let child = lower_owned(expression, aggregates, subqueries, control)?;
        OwnedScalarExpr::attach_child(self, child)
    }
}

fn lower_owned(
    expression: Source<'_, Expr>,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut dyn QueryLowering,
    control: ProductionControl<'_>,
) -> Result<OwnedScalarExpr> {
    control.check()?;
    let mut lowering = Lowering {
        control: control.budget().map(|_| Control::from_production(control)),
        owned_children: control.budget().map(BudgetedVec::new),
    };
    let expression = lowering.expression_node(expression, aggregates, subqueries)?;
    let output = OwnedScalarExpr::from_rewrite_parts(
        expression,
        lowering.control.map(|control| ScalarMemory {
            direct: control.memory,
            children: lowering.owned_children.expect("mutable scalar lowering"),
        }),
    );
    control.check()?;
    Ok(output)
}

pub(in crate::plan) fn lower_scalar_with_arena(
    expression: &Expr,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut dyn QueryLowering,
    control: ProductionControl<'_>,
) -> Result<OwnedScalarExpr> {
    lower_owned(
        Source::Borrowed(expression),
        aggregates,
        subqueries,
        control,
    )
}

#[cfg(test)]
mod tests;
