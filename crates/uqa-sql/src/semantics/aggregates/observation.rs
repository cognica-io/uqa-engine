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
    control.check()?;
    if classify(expr)? {
        return observe(expr);
    }
    match expr {
        ScalarExpr::Func { args, filter, .. } => {
            for arg in args {
                visit(arg, control, classify, observe)?;
            }
            if let Some(filter) = filter.as_deref() {
                visit(filter, control, classify, observe)?;
            }
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                visit(item, control, classify, observe)?;
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            visit(lhs, control, classify, observe)?;
            visit(rhs, control, classify, observe)?;
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => {
            visit(inner, control, classify, observe)?;
        }
        ScalarExpr::IsNull { expr, .. } => visit(expr, control, classify, observe)?,
        ScalarExpr::Between { expr, low, high } => {
            visit(expr, control, classify, observe)?;
            visit(low, control, classify, observe)?;
            visit(high, control, classify, observe)?;
        }
        ScalarExpr::InList { expr, list, .. } => {
            visit(expr, control, classify, observe)?;
            for item in list {
                visit(item, control, classify, observe)?;
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_deref() {
                visit(base, control, classify, observe)?;
            }
            for (condition, result) in when {
                visit(condition, control, classify, observe)?;
                visit(result, control, classify, observe)?;
            }
            if let Some(else_branch) = else_branch.as_deref() {
                visit(else_branch, control, classify, observe)?;
            }
        }
        ScalarExpr::InSubquery { expr, .. } => visit(expr, control, classify, observe)?,
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
    Ok(())
}
