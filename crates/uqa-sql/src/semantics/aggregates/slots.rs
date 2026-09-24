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
    rewrite_selected(expr, &mut |expression| {
        if !is_aggregate(context, expression) {
            return Ok(None);
        }
        let slot = aggregate_slot(relation, *cursor);
        *cursor += 1;
        Ok(Some(slot))
    })
}

pub fn compile_having_aggregate_slots(
    context: &dyn AggregateClassifier,
    expr: &ScalarExpr,
    relation: crate::ast::InternalRelationId,
    aggregate_targets: &[ScalarExpr],
) -> Result<ScalarExpr, SQLError> {
    rewrite_selected(expr, &mut |aggregate| {
        if !is_aggregate(context, aggregate) {
            return Ok(None);
        }
        aggregate_targets
            .iter()
            .position(|target| exprs_match(target, aggregate))
            .map(|index| Some(aggregate_slot(relation, index)))
            .ok_or_else(|| {
                SQLError::Unsupported(
                    "HAVING references an aggregate that is not in the aggregate plan".into(),
                )
            })
    })
}

/// Group values follow aggregate finalizers in the retained scalar slot namespace. Match a complete grouped expression before descending into its inputs.
pub fn compile_group_slots(
    expression: &ScalarExpr,
    groups: &[ScalarExpr],
    relation: crate::ast::InternalRelationId,
    first_slot: usize,
) -> Result<ScalarExpr, SQLError> {
    rewrite_selected(expression, &mut |expression| {
        groups
            .iter()
            .position(|group| exprs_match(group, expression))
            .map(|index| {
                first_slot
                    .checked_add(index)
                    .map(|index| aggregate_slot(relation, index))
                    .ok_or_else(|| SQLError::Internal("aggregate scalar slot overflow".into()))
            })
            .transpose()
    })
}

/// An absent grouping-set key is NULL as a complete expression, even when another selected key exposes one of its inputs. Aggregate arguments still read the original input rows.
pub fn select_grouping_set(
    context: &dyn AggregateClassifier,
    statement: &crate::plan::QueryBlockPlan,
    selected: &[ScalarExpr],
) -> Result<crate::plan::QueryBlockPlan, SQLError> {
    let groups = statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
        .collect::<Vec<_>>();
    let mut active = statement.clone();
    active.group_by = selected.to_vec();
    active.grouping_sets.clear();
    let mut rewrite = |expression: &ScalarExpr| {
        if is_aggregate(context, expression) {
            return Ok(Some(expression.clone()));
        }
        Ok(groups
            .iter()
            .any(|group| exprs_match(group, expression))
            .then(|| {
                if selected.iter().any(|group| exprs_match(group, expression)) {
                    expression.clone()
                } else {
                    ScalarExpr::Literal(uqa_core::Value::Null)
                }
            }))
    };
    for projection in &mut active.projections {
        projection.expr = rewrite_selected(&projection.expr, &mut rewrite)?;
    }
    if let Some(having) = &mut active.having {
        *having = rewrite_selected(having, &mut rewrite)?;
    }
    Ok(active)
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
fn rewrite_selected(
    expr: &ScalarExpr,
    replace: &mut impl FnMut(&ScalarExpr) -> Result<Option<ScalarExpr>, SQLError>,
) -> Result<ScalarExpr, SQLError> {
    if let Some(expression) = replace(expr)? {
        return Ok(expression);
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
                .map(|arg| rewrite_selected(arg, replace))
                .collect::<Result<Vec<_>, _>>()?,
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_deref()
                .map(|filter| rewrite_selected(filter, replace).map(Box::new))
                .transpose()?,
        }),
        ScalarExpr::Array(items) => Ok(ScalarExpr::Array(
            items
                .iter()
                .map(|item| rewrite_selected(item, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Row(items) => Ok(ScalarExpr::Row(
            items
                .iter()
                .map(|item| rewrite_selected(item, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Binary { op, lhs, rhs } => Ok(ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(rewrite_selected(lhs, replace)?),
            rhs: Box::new(rewrite_selected(rhs, replace)?),
        }),
        ScalarExpr::Not(inner) => Ok(ScalarExpr::Not(Box::new(rewrite_selected(inner, replace)?))),
        ScalarExpr::UnaryMinus(inner) => Ok(ScalarExpr::UnaryMinus(Box::new(rewrite_selected(
            inner, replace,
        )?))),
        ScalarExpr::And(parts) => Ok(ScalarExpr::And(
            parts
                .iter()
                .map(|part| rewrite_selected(part, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Or(parts) => Ok(ScalarExpr::Or(
            parts
                .iter()
                .map(|part| rewrite_selected(part, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::IsNull { expr, negated } => Ok(ScalarExpr::IsNull {
            expr: Box::new(rewrite_selected(expr, replace)?),
            negated: *negated,
        }),
        ScalarExpr::Between { expr, low, high } => Ok(ScalarExpr::Between {
            expr: Box::new(rewrite_selected(expr, replace)?),
            low: Box::new(rewrite_selected(low, replace)?),
            high: Box::new(rewrite_selected(high, replace)?),
        }),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => Ok(ScalarExpr::InList {
            expr: Box::new(rewrite_selected(expr, replace)?),
            list: list
                .iter()
                .map(|item| rewrite_selected(item, replace))
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
                .map(|base| rewrite_selected(base, replace).map(Box::new))
                .transpose()?,
            when: when
                .iter()
                .map(|(condition, result)| {
                    Ok((
                        rewrite_selected(condition, replace)?,
                        rewrite_selected(result, replace)?,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            else_branch: else_branch
                .as_deref()
                .map(|branch| rewrite_selected(branch, replace).map(Box::new))
                .transpose()?,
        }),
        ScalarExpr::Cast { expr, ty } => Ok(ScalarExpr::Cast {
            expr: Box::new(rewrite_selected(expr, replace)?),
            ty: ty.clone(),
        }),
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(ScalarExpr::InSubquery {
            expr: Box::new(rewrite_selected(expr, replace)?),
            subquery: *subquery,
            negated: *negated,
        }),
        other => Ok(other.clone()),
    }
}
