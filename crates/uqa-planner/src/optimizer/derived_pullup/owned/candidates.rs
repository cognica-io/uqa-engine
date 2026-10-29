//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Derived capture retains producer qualification and publishes only the demanded opaque input attributes.

use super::{
    Buffer, Context, MutableQueryPlan, MutableSourcePlan, OwnedScalarExpr, ProductionControl,
    RelationalPlan, SQLError, ScalarExpr, SourcePlan,
};
use uqa_core::memory::{Produced, ProductionMap, ProductionVec};
use uqa_sql::{
    plan::{AnalyzedSource, ComputePlan, OwnedProjection},
    ColumnIdentity, RowSchema,
};

pub(super) struct Lifted {
    pub(super) qualifier: Produced<String>,
    pub(super) source: MutableSourcePlan,
    pub(super) outputs: Buffer<(Produced<ColumnIdentity>, OwnedScalarExpr)>,
}

pub(super) fn collect(
    source: &SourcePlan,
    analyzed: &AnalyzedSource,
    context: &Context<'_>,
    predicates: &mut Buffer<OwnedScalarExpr>,
    output: &mut Buffer<Lifted>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let SourcePlan::Join { left, right, .. } = source {
        if let Some(children) = &analyzed.children {
            collect(left, &children.0, context, predicates, output, control)?;
            collect(right, &children.1, context, predicates, output, control)?;
        }
    } else if let Some(candidate) = candidate(source, analyzed, context, predicates, control)? {
        output.push(candidate)?;
    }
    Ok(())
}

fn candidate(
    source: &SourcePlan,
    analyzed: &AnalyzedSource,
    context: &Context<'_>,
    predicates: &mut Buffer<OwnedScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<Option<Lifted>, SQLError> {
    let SourcePlan::Subquery {
        body,
        captured_output: None,
        alias: Some(qualifier),
        lateral: false,
        ..
    } = source
    else {
        return Ok(None);
    };
    let Some(input) = &body.analyzed_source else {
        return Ok(None);
    };
    let RelationalPlan::QueryBlock(block) = &body.root else {
        return Ok(None);
    };
    if !eligible_body(body, block, context, control)? {
        return Ok(None);
    }
    if block.from.is_none() || input.schema.columns_are_open(None) {
        return Ok(None);
    }
    let mut unique = ProductionMap::new(*control);
    for identity in analyzed.schema.identities() {
        unique.insert_copy(identity, ())?;
    }
    if unique.len() != analyzed.schema.len() {
        return Ok(None);
    }
    drop(unique);
    let changed = body.copy_for_rewrite(control)?;
    let (changed, expanded) = super::super::super::derived_projection::owned::expand_stars(
        changed,
        Some(&analyzed.schema),
        control,
    )?;
    if !expanded || super::projections(&changed).len() != analyzed.schema.len() {
        return Ok(None);
    }
    let (mut changed, qualified) = super::transforms::qualify(changed, &input.schema, control)?;
    if !qualified {
        return Ok(None);
    }
    let Some(captured) = capture(&changed, &input.schema, &analyzed.schema, control)? else {
        return Ok(None);
    };
    changed = move_predicates(
        changed,
        qualifier,
        analyzed.schema.columns(),
        context,
        predicates,
        control,
    )?;
    let mut projections = Buffer::new(*control);
    for position in captured.positions.iter().copied() {
        let identity = &input.schema.identities()[position];
        projections.push(OwnedProjection {
            expression: super::identity_expression(identity, control)?,
            alias: Some(control.copy_text(identity.column())?),
        })?;
    }
    changed = changed.replace_projections(projections.into_iter(), control)?;
    let output = input.schema.for_selected_result_columns_with_control(
        captured
            .positions
            .iter()
            .map(|position| (input.schema.columns()[*position].as_str(), *position)),
        control,
    )?;
    changed = changed.set_analyzed_output(Some(output), control)?;
    let source = MutableSourcePlan::subquery(
        changed,
        Some(control.copy_text(qualifier)?),
        control.finish(Vec::new(), control.empty_reservation())?,
        Some(captured.relation),
        false,
        control,
    )?;
    Ok(Some(Lifted {
        qualifier: control.copy_text(qualifier)?,
        source,
        outputs: captured.outputs,
    }))
}

fn eligible_body(
    body: &uqa_sql::plan::QueryPlan,
    block: &uqa_sql::plan::QueryBlockPlan,
    context: &Context<'_>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if body.relations_bound != context.relations_bound
        || body.relation_privilege_subject.is_some()
        || !body.ctes.is_empty()
        || !block.subqueries.is_empty()
        || !matches!(block.compute, ComputePlan::Project)
        || block.distinct
        || !block.distinct_on.is_empty()
        || !block.group_by.is_empty()
        || !block.grouping_sets.is_empty()
        || block.having.is_some()
        || !block.windows.is_empty()
        || !block.order_by.is_empty()
        || block.limit.is_some()
        || block.offset.is_some()
        || !block.locking.is_empty()
        || uqa_sql::semantics::volatility::query_contains_volatile_function_with_control(
            context.volatility,
            body,
            control,
        )?
    {
        return Ok(false);
    }
    for projection in &block.projections {
        if uqa_sql::semantics::sets::static_setness::expression_may_return_set_statically_with_control(context.routines, &projection.expr, control)? {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Capture {
    relation: uqa_sql::ast::InternalRelationId,
    positions: Produced<Vec<usize>>,
    outputs: Buffer<(Produced<ColumnIdentity>, OwnedScalarExpr)>,
}

fn capture(
    query: &MutableQueryPlan,
    input: &RowSchema,
    output: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Option<Capture>, SQLError> {
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let mut positions = ProductionVec::new(*control);
    let mut attributes = ProductionMap::new(*control);
    let mut outputs = Buffer::new(*control);
    for (identity, projection) in output.identities().iter().zip(super::projections(query)) {
        let mut valid = true;
        let expression =
            projection
                .expr
                .copy_for_rewrite(control)?
                .replace_nodes(control, &mut |node| match node {
                    ScalarExpr::QualifiedColumn { qualifier, column } => {
                        let Some(position) = input.qualified_position(qualifier, column) else {
                            valid = false;
                            return Ok(None);
                        };
                        let attribute = match attributes.get(&position) {
                            Some(attribute) => *attribute,
                            None => {
                                let attribute = positions.len();
                                positions.push_copy(position)?;
                                attributes.insert_copy(position, attribute)?;
                                attribute
                            }
                        };
                        Ok(Some(
                            ScalarExpr::InternalColumn(relation.column(attribute))
                                .copy_for_rewrite(control)?,
                        ))
                    }
                    ScalarExpr::Column(_) | ScalarExpr::InternalColumn(_) => {
                        valid = false;
                        Ok(None)
                    }
                    _ => Ok(None),
                })?;
        if !valid {
            return Ok(None);
        }
        outputs.push((identity.clone_with_control(control)?, expression))?;
    }
    Ok(Some(Capture {
        relation,
        positions: positions.finish()?,
        outputs,
    }))
}

fn move_predicates(
    mut query: MutableQueryPlan,
    qualifier: &str,
    columns: &[String],
    context: &Context<'_>,
    predicates: &mut Buffer<OwnedScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let mut retained = Buffer::new(*control);
    for predicate in std::mem::replace(predicates, Buffer::new(*control)) {
        let mut used = false;
        let rewritten = if !uqa_sql::semantics::expr_contains_subquery(&predicate)
            && !uqa_sql::semantics::volatility::expr_contains_volatile_function_with_control(
                context.volatility,
                &predicate,
                control,
            )? {
            crate::filter_pushdown::owned::rewrite_output_filter(
                &predicate,
                qualifier,
                columns,
                super::projections(&query),
                &mut |_| {
                    used = true;
                    Ok(())
                },
                false,
                control,
            )?
        } else {
            None
        };
        if let Some(rewritten) = rewritten.filter(|_| used) {
            query = super::append_where(query, rewritten, control)?;
            drop(predicate);
        } else {
            retained.push(predicate)?;
        }
    }
    *predicates = retained;
    Ok(query)
}
