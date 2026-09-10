//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assign internal scalar slots to projection and HAVING aggregates.

use super::{exprs_match, is_aggregate};
use crate::plan::AggregateClassifier;
use crate::{SQLError, ScalarExpr};

pub fn compile_projection_aggregate_slots(
    context: &dyn AggregateClassifier,
    expr: &ScalarExpr,
    relation: crate::ast::InternalRelationId,
    cursor: &mut usize,
) -> Result<ScalarExpr, SQLError> {
    rewrite_aggregates(context, expr, &mut |_| {
        let slot = aggregate_slot(relation, *cursor);
        *cursor += 1;
        Ok(slot)
    })
}

pub fn compile_having_aggregate_slots(
    context: &dyn AggregateClassifier,
    expr: &ScalarExpr,
    relation: crate::ast::InternalRelationId,
    aggregate_targets: &[ScalarExpr],
) -> Result<ScalarExpr, SQLError> {
    rewrite_aggregates(context, expr, &mut |aggregate| {
        aggregate_targets
            .iter()
            .position(|target| exprs_match(target, aggregate))
            .map(|index| aggregate_slot(relation, index))
            .ok_or_else(|| {
                SQLError::Unsupported(
                    "HAVING references an aggregate that is not in the aggregate plan".into(),
                )
            })
    })
}

pub fn aggregate_slot_index(
    column: crate::ast::InternalColumnRef,
    relation: crate::ast::InternalRelationId,
) -> Option<usize> {
    (column.relation() == relation).then(|| column.attribute())
}

fn aggregate_slot(relation: crate::ast::InternalRelationId, index: usize) -> ScalarExpr {
    ScalarExpr::InternalColumn(relation.column(index))
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves aggregate NULL and type order"
)]
fn rewrite_aggregates(
    context: &dyn AggregateClassifier,
    expr: &ScalarExpr,
    replace: &mut impl FnMut(&ScalarExpr) -> Result<ScalarExpr, SQLError>,
) -> Result<ScalarExpr, SQLError> {
    if is_aggregate(context, expr) {
        return replace(expr);
    }
    match expr {
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => Ok(ScalarExpr::Func {
            name: name.clone(),
            binding: binding.clone(),
            args: args
                .iter()
                .map(|arg| rewrite_aggregates(context, arg, replace))
                .collect::<Result<Vec<_>, _>>()?,
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_deref()
                .map(|filter| rewrite_aggregates(context, filter, replace).map(Box::new))
                .transpose()?,
        }),
        ScalarExpr::Array(items) => Ok(ScalarExpr::Array(
            items
                .iter()
                .map(|item| rewrite_aggregates(context, item, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Row(items) => Ok(ScalarExpr::Row(
            items
                .iter()
                .map(|item| rewrite_aggregates(context, item, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Binary { op, lhs, rhs } => Ok(ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(rewrite_aggregates(context, lhs, replace)?),
            rhs: Box::new(rewrite_aggregates(context, rhs, replace)?),
        }),
        ScalarExpr::Not(inner) => Ok(ScalarExpr::Not(Box::new(rewrite_aggregates(
            context, inner, replace,
        )?))),
        ScalarExpr::UnaryMinus(inner) => Ok(ScalarExpr::UnaryMinus(Box::new(rewrite_aggregates(
            context, inner, replace,
        )?))),
        ScalarExpr::And(parts) => Ok(ScalarExpr::And(
            parts
                .iter()
                .map(|part| rewrite_aggregates(context, part, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Or(parts) => Ok(ScalarExpr::Or(
            parts
                .iter()
                .map(|part| rewrite_aggregates(context, part, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::IsNull { expr, negated } => Ok(ScalarExpr::IsNull {
            expr: Box::new(rewrite_aggregates(context, expr, replace)?),
            negated: *negated,
        }),
        ScalarExpr::Between { expr, low, high } => Ok(ScalarExpr::Between {
            expr: Box::new(rewrite_aggregates(context, expr, replace)?),
            low: Box::new(rewrite_aggregates(context, low, replace)?),
            high: Box::new(rewrite_aggregates(context, high, replace)?),
        }),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => Ok(ScalarExpr::InList {
            expr: Box::new(rewrite_aggregates(context, expr, replace)?),
            list: list
                .iter()
                .map(|item| rewrite_aggregates(context, item, replace))
                .collect::<Result<Vec<_>, _>>()?,
            negated: *negated,
        }),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => Ok(ScalarExpr::Case {
            base: base
                .as_deref()
                .map(|base| rewrite_aggregates(context, base, replace).map(Box::new))
                .transpose()?,
            when: when
                .iter()
                .map(|(condition, result)| {
                    Ok((
                        rewrite_aggregates(context, condition, replace)?,
                        rewrite_aggregates(context, result, replace)?,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            else_branch: else_branch
                .as_deref()
                .map(|branch| rewrite_aggregates(context, branch, replace).map(Box::new))
                .transpose()?,
        }),
        ScalarExpr::Cast { expr, ty } => Ok(ScalarExpr::Cast {
            expr: Box::new(rewrite_aggregates(context, expr, replace)?),
            ty: ty.clone(),
        }),
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(ScalarExpr::InSubquery {
            expr: Box::new(rewrite_aggregates(context, expr, replace)?),
            subquery: *subquery,
            negated: *negated,
        }),
        other => Ok(other.clone()),
    }
}
