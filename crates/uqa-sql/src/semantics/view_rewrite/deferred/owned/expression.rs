//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A selected outer expression moves its own query occurrences into the consumer namespace exactly once.

use super::{namespaces, DeferredOuterRewrite, SQLError};
use crate::{
    plan::{
        MutableExecutablePlan, MutableExpressionPlan, MutableQueryPlan, OwnedScalarExpr,
        QueryChildSlot, RelationalPlan, ScalarQueryArena,
    },
    production::Buffer,
    ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub(super) fn node(
    rewrite: &DeferredOuterRewrite,
    expression: OwnedScalarExpr,
    arena: &mut ScalarQueryArena,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    control.check()?;
    if let ScalarExpr::ScalarSubquery(id)
    | ScalarExpr::Exists { subquery: id, .. }
    | ScalarExpr::InSubquery { subquery: id, .. } = &*expression
    {
        if *id < arena.queries().len() {
            arena.map_query(*id, control, |query| self::query(rewrite, query, control))?;
        }
        return Ok(expression);
    }
    let identity = match &*expression {
        ScalarExpr::Column(column) if !rewrite.shadowed_columns.contains(column.as_str()) => {
            (None, column.as_str())
        }
        ScalarExpr::QualifiedStar(qualifier)
            if !rewrite.shadowed_qualifiers.contains(qualifier.as_str()) =>
        {
            (None, qualifier.as_str())
        }
        ScalarExpr::QualifiedColumn { qualifier, column }
            if !rewrite.shadowed_qualifiers.contains(qualifier.as_str()) =>
        {
            (Some(qualifier.as_str()), column.as_str())
        }
        _ => return Ok(expression),
    };
    let Some((_, replacement)) = rewrite
        .replacements
        .iter()
        .find(|(key, _)| key.qualifier() == identity.0 && key.column() == identity.1)
    else {
        return Ok(expression);
    };
    let qualifier = replacement.qualifier.as_str();
    let mut replacement = replacement.expression.copy_for_rewrite(control)?;
    let bind = |expression: OwnedScalarExpr| -> Result<OwnedScalarExpr, SQLError> {
        expression.transform_nodes(control, &mut |_, expression| {
            bind_column(rewrite, qualifier, expression, control)
        })
    };
    replacement = replacement.map_scalar(control, |expression, _| bind(expression))?;
    for index in 0..replacement.queries().len() {
        replacement = replacement.map_query(index, control, |query| {
            query.transform_scalar_nodes(control, &mut |_, expression| {
                bind_column(rewrite, qualifier, expression, control)
            })
        })?;
    }
    arena.append_expression(replacement, control)
}

fn bind_column(
    rewrite: &DeferredOuterRewrite,
    qualifier: &str,
    expression: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    if let ScalarExpr::QualifiedColumn {
        qualifier: name,
        column,
    } = &*expression
    {
        if name == qualifier {
            if let Some((_, reference)) = rewrite.outer_columns.iter().find(|(identity, _)| {
                identity.qualifier() == Some(name.as_str()) && identity.column() == column.as_str()
            }) {
                return Ok(ScalarExpr::InternalColumn(*reference).copy_for_rewrite(control)?);
            }
        }
    }
    Ok(expression)
}

pub(super) fn query(
    rewrite: &DeferredOuterRewrite,
    mut query: MutableQueryPlan,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    for index in 0..query.ctes.len() {
        query = query.map_cte(index, control, |body| {
            Ok(match body {
                MutableExecutablePlan::Query(body) => {
                    MutableExecutablePlan::Query(self::query(rewrite, body, control)?)
                }
                body => body,
            })
        })?;
    }
    match &query.root {
        RelationalPlan::QueryBlock(_) => {
            let (query, existing) = query.take_deferred_rewrites(control)?;
            let mut output = Buffer::new(control);
            let mut found = false;
            for previous in existing {
                if std::sync::Arc::ptr_eq(&previous.outer_columns, &rewrite.outer_columns) {
                    found = true;
                    output.push(namespaces::merged(&previous, rewrite, control)?, control)?;
                } else {
                    output.push(previous, control)?;
                }
            }
            if !found {
                output.push(rewrite.clone_with_control(control)?, control)?;
            }
            query.replace_deferred_rewrites(output.into_iter(), control)
        }
        RelationalPlan::SetOp { .. } => {
            query = query.map_query_child(QueryChildSlot::Left, control, |query| {
                self::query(rewrite, query, control)
            })?;
            query = query.map_query_child(QueryChildSlot::Right, control, |query| {
                self::query(rewrite, query, control)
            })?;
            query.transform_root_scalar_arenas(control, &mut |_, expression, arena| {
                node(rewrite, expression, arena, control)
            })
        }
        RelationalPlan::Values { .. } => query
            .transform_root_scalar_arenas(control, &mut |_, expression, arena| {
                node(rewrite, expression, arena, control)
            }),
    }
}

impl DeferredOuterRewrite {
    pub fn bind_expression_owned(
        &self,
        expression: MutableExpressionPlan,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExpressionPlan, SQLError> {
        expression.map_scalar(control, |expression, arena| {
            expression.transform_nodes(control, &mut |_, expression| {
                node(self, expression, arena, control)
            })
        })
    }
}
