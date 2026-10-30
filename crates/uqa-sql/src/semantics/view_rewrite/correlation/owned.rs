//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlated rewrites consume each private query while retaining its lexical replacement owners.

use super::CorrelatedRewriteContext;
use crate::{
    plan::{
        MutableExecutablePlan, MutableExpressionPlan, MutableQueryPlan, OwnedScalarExpr,
        QueryChildSlot, RelationalPlan, ScalarQueryArena,
    },
    production::Buffer,
    semantics::view_rewrite::{layer_column, layer_rewrite::embed_owned, DeferredOuterRewrite},
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub(in crate::semantics::view_rewrite) fn query(
    context: &CorrelatedRewriteContext<'_>,
    mut input: MutableQueryPlan,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    for index in 0..input.ctes.len() {
        input = input.map_cte(index, control, |input| match input {
            MutableExecutablePlan::Query(input) => Ok(MutableExecutablePlan::Query(query(
                context, input, control,
            )?)),
            MutableExecutablePlan::Command(_) => Err(SQLError::Unsupported(
                "WITH clause containing a data-modifying statement must be at the top level".into(),
            )),
        })?;
    }
    match &input.root {
        RelationalPlan::QueryBlock(block) => {
            if block.deferred_outer_rewrites.is_empty() {
                let rewrite = DeferredOuterRewrite::capture_owned(context, block, control)?;
                input.replace_deferred_rewrites(std::iter::once(rewrite), control)
            } else {
                let (input, rewrites) = input.take_deferred_rewrites(control)?;
                let mut output = Buffer::new(control);
                for rewrite in rewrites {
                    output.push(rewrite.rebase_owned(context, control)?, control)?;
                }
                input.replace_deferred_rewrites(output.into_iter(), control)
            }
        }
        RelationalPlan::SetOp { .. } | RelationalPlan::Values { .. } => {
            if matches!(input.root, RelationalPlan::SetOp { .. }) {
                input = input.map_query_child(QueryChildSlot::Left, control, |input| {
                    query(context, input, control)
                })?;
                input = input.map_query_child(QueryChildSlot::Right, control, |input| {
                    query(context, input, control)
                })?;
            }
            let count = match &input.root {
                RelationalPlan::SetOp { subqueries, .. }
                | RelationalPlan::Values { subqueries, .. } => subqueries.len(),
                RelationalPlan::QueryBlock(_) => unreachable!(),
            };
            for index in 0..count {
                input =
                    input.map_query_child(QueryChildSlot::Subquery(index), control, |input| {
                        query(context, input, control)
                    })?;
            }
            input.transform_root_scalar_arenas(control, &mut |_, input, arena| {
                node(context, input, arena, control)
            })
        }
    }
}

pub(in crate::semantics::view_rewrite) fn expression(
    context: &CorrelatedRewriteContext<'_>,
    mut input: MutableExpressionPlan,
    control: &ProductionControl<'_>,
) -> Result<MutableExpressionPlan, SQLError> {
    for index in 0..input.queries().len() {
        input = input.map_query(index, control, |input| query(context, input, control))?;
    }
    input.map_scalar(control, |input, arena| {
        input.transform_nodes(control, &mut |_, input| {
            node(context, input, arena, control)
        })
    })
}

fn node(
    context: &CorrelatedRewriteContext<'_>,
    input: OwnedScalarExpr,
    arena: &mut ScalarQueryArena,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let (column, qualifier) = match &*input {
        ScalarExpr::Column(column) => (column, context.default_target_qualifier),
        ScalarExpr::QualifiedColumn { qualifier, column }
            if context.target_qualifiers.contains(qualifier) =>
        {
            (column, qualifier.as_str())
        }
        _ => return Ok(input),
    };
    if let Some(mapping) = layer_column(context.layer, column) {
        return arena.append_expression(
            embed_owned(
                context.services,
                &mapping.expression,
                context.layer,
                qualifier,
                control,
            )?,
            control,
        );
    }
    if matches!(&*input, ScalarExpr::Column(_)) {
        if let Some(qualifier) = context
            .source
            .and_then(|source| {
                source
                    .unqualified_position(column)
                    .and_then(|position| source.identity(position))
            })
            .and_then(crate::ColumnIdentity::qualifier)
        {
            return Ok(OwnedScalarExpr::positioned_column(
                control.copy_text(column)?,
                Some(control.copy_text(qualifier)?),
                column.position(),
                control,
            )?);
        }
    }
    Ok(input)
}
