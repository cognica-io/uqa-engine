//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed aggregate traversal shares ordering without allocating a temporary target list.

use crate::{
    plan::{AggregateClassifier, ProjectionPlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub fn is_aggregate_with_control(
    aggregates: &dyn AggregateClassifier,
    expr: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if super::is_builtin_aggregate(expr) {
        return Ok(true);
    }
    match expr {
        ScalarExpr::Func { name, .. } => {
            aggregates.is_registered_aggregate_with_control(name, control)
        }
        _ => Ok(false),
    }
}

pub fn has_aggregate_with_control(
    aggregates: &dyn AggregateClassifier,
    projections: &[ProjectionPlan],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    for projection in projections {
        if contains_aggregate_with_control(aggregates, &projection.expr, control)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn contains_aggregate_with_control(
    aggregates: &dyn AggregateClassifier,
    expr: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut found = false;
    visit_aggregate_exprs_with_control(aggregates, expr, control, &mut |_| {
        found = true;
        Ok(())
    })?;
    Ok(found)
}

pub fn visit_aggregate_exprs_with_control<'a>(
    aggregates: &dyn AggregateClassifier,
    expr: &'a ScalarExpr,
    control: &ProductionControl<'_>,
    observe: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    visit(
        expr,
        control,
        &mut |expr| is_aggregate_with_control(aggregates, expr, control),
        observe,
    )
}

pub(super) fn visit_ordinary<'a>(
    aggregates: &dyn AggregateClassifier,
    expr: &'a ScalarExpr,
    observe: &mut dyn FnMut(&'a ScalarExpr),
) {
    visit(
        expr,
        &ProductionControl::uncontrolled(),
        &mut |expr| Ok(super::is_aggregate(aggregates, expr)),
        &mut |expr| {
            observe(expr);
            Ok(())
        },
    )
    .expect("ordinary aggregate observation");
}

fn visit<'a>(
    expr: &'a ScalarExpr,
    control: &ProductionControl<'_>,
    classify: &mut dyn FnMut(&ScalarExpr) -> Result<bool, SQLError>,
    observe: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(visit_with_work(expr, control, &mut (), classify, observe))
}

async fn visit_with_work<'a, W, C, O>(
    expr: &'a ScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
    classify: &mut C,
    observe: &mut O,
) -> Result<(), SQLError>
where
    W: crate::catalog::work::Work,
    C: FnMut(&ScalarExpr) -> Result<bool, SQLError> + ?Sized,
    O: FnMut(&'a ScalarExpr) -> Result<(), SQLError> + ?Sized,
{
    use crate::ir::observation::Decision;
    crate::ir::observation::visit(expr, control, work, false, &mut |expr| {
        if classify(expr)? {
            observe(expr)?;
            Ok(Decision::Prune)
        } else {
            Ok(Decision::Descend)
        }
    })
    .await?;
    Ok(())
}

pub(crate) async fn contains_with_work<W, C>(
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
    classify: &mut C,
) -> Result<bool, SQLError>
where
    W: crate::catalog::work::Work,
    C: FnMut(&ScalarExpr) -> Result<bool, SQLError> + ?Sized,
{
    let mut found = false;
    visit_with_work(expression, control, work, classify, &mut |_| {
        found = true;
        Ok(())
    })
    .await?;
    Ok(found)
}
