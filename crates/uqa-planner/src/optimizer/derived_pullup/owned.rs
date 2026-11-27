//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A private derived-query candidate retains exact owners until every scope-sensitive replacement succeeds.

use super::{ordinary_inner_source, Context, OptimizerConfig};
use crate::scratch::Buffer;
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    plan::{
        CommandPlan, MutableExecutablePlan, MutableQueryPlan, MutableSourcePlan, OwnedScalarExpr,
        QueryChildSlot, RelationalPlan, SourceChildSlot, SourcePlan,
    },
    ColumnIdentity, RowSchema, SQLError, ScalarExpr,
};

mod candidates;
mod predicates;
mod transforms;

pub(in crate::optimizer) fn rewrite(
    plan: MutableExecutablePlan,
    config: &OptimizerConfig<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    let Some(catalog) = &config.routine_inlining else {
        return Ok(plan);
    };
    if matches!(&plan, MutableExecutablePlan::Command(command) if matches!(&**command,
        CommandPlan::CreateView { .. } | CommandPlan::CreateMaterializedView { .. }
        | CommandPlan::Prepare { .. } | CommandPlan::Explain { .. }))
    {
        return Ok(plan);
    }
    let context = Context {
        routines: catalog.routines,
        volatility: catalog.volatility,
        relations_bound: false,
    };
    plan.map_queries_while(&config.production, &mut |query| {
        let query = query_block(query, &context, &config.production)?;
        Ok((predicates::push(query, &context, &config.production)?, true))
    })
}

pub(super) fn query_block(
    query: MutableQueryPlan,
    context: &Context<'_>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    let context = Context {
        relations_bound: query.relations_bound,
        ..*context
    };
    let (Some(input), Some(output)) =
        (query.analyzed_source.clone(), query.analyzed_output.clone())
    else {
        return Ok(query);
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(query);
    };
    if block.r#where.is_none()
        || !block.locking.is_empty()
        || !block.privilege_columns.is_empty()
        || input.schema.columns_are_open(None)
        || !block.from.as_ref().is_some_and(ordinary_inner_source)
    {
        return Ok(query);
    }
    let source = block.from.as_ref().expect("selected inner source");
    if !candidates::has_candidate(source, &input, &context, control)? {
        return Ok(query);
    }
    let changed = query.copy_for_rewrite(control)?;
    let (changed, expanded) =
        super::super::derived_projection::owned::expand_stars(changed, Some(&output), control)?;
    if !expanded || projections(&changed).len() != output.len() {
        return Ok(query);
    }
    let (mut changed, qualified) = transforms::qualify(changed, &input.schema, control)?;
    if !qualified {
        return Ok(query);
    }
    for (index, name) in output.columns().iter().enumerate() {
        changed =
            changed.replace_projection_alias(index, Some(control.copy_text(name)?), control)?;
    }
    let (mut changed, predicate) = changed.take_where(control)?;
    let mut predicates = Buffer::new(*control);
    if let Some(predicate) = predicate {
        flatten(predicate, &mut predicates, control)?;
    }
    let mut lifted = Buffer::new(*control);
    let RelationalPlan::QueryBlock(block) = &changed.root else {
        unreachable!()
    };
    candidates::collect(
        block.from.as_ref().expect("selected source"),
        &input,
        &context,
        &mut predicates,
        &mut lifted,
        control,
    )?;
    if lifted.is_empty() {
        return Ok(query);
    }
    changed = changed.replace_where(combine(predicates, control)?, control)?;
    // Capture produces closed expressions containing opaque relation attributes. An earlier source owns a duplicate identity just as it did before later sources were substituted.
    let outputs = lifted
        .iter()
        .rev()
        .flat_map(|candidate| candidate.outputs.iter())
        .map(|(identity, expression)| (&**identity, &**expression));
    let Some(substitutions) =
        uqa_sql::binding::scoped_references::OuterColumnSubstitutions::new(outputs, control)?
    else {
        return Ok(query);
    };
    let RelationalPlan::QueryBlock(block) = &changed.root else {
        unreachable!()
    };
    for index in 0..block.subqueries.len() {
        let mut complete = true;
        changed = changed.map_query_child(QueryChildSlot::Subquery(index), control, |child| {
            let (child, rewritten) = substitutions.substitute(child, &input.schema, control)?;
            complete = rewritten;
            Ok(child)
        })?;
        if !complete {
            return Ok(query);
        }
    }
    changed = transforms::local(changed, control, &mut |expression| {
        expression.replace_nodes(control, &mut |node| {
            let replacement = match node {
                ScalarExpr::Column(name) => substitutions.get(None, name),
                ScalarExpr::QualifiedColumn { qualifier, column } => {
                    substitutions.get(Some(qualifier), column)
                }
                _ => None,
            };
            replacement
                .map(|value| value.copy_for_rewrite(control).map_err(Into::into))
                .transpose()
        })
    })?;
    drop(substitutions);
    for candidate in lifted {
        changed = changed.map_source(control, |source| {
            replace_source(source, &candidate.qualifier, candidate.source, control)
        })?;
    }
    drop(query);
    Ok(changed)
}

fn projections(query: &MutableQueryPlan) -> &[uqa_sql::plan::ProjectionPlan] {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    &block.projections
}

fn identity_expression(
    identity: &ColumnIdentity,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    Ok(OwnedScalarExpr::column(
        control.copy_text(identity.column())?,
        identity
            .qualifier()
            .map(|name| control.copy_text(name))
            .transpose()?,
        control,
    )?)
}

fn flatten(
    expression: OwnedScalarExpr,
    output: &mut Buffer<OwnedScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if matches!(&*expression, ScalarExpr::And(_)) {
        for child in expression.into_children() {
            flatten(child, output, control)?;
        }
    } else {
        output.push(expression)?;
    }
    Ok(())
}

fn combine(
    mut parts: Buffer<OwnedScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    Ok(match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(OwnedScalarExpr::boolean(parts.into_iter(), true, control)?),
    })
}

fn append_where(
    query: MutableQueryPlan,
    expression: OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let (query, previous) = query.take_where(control)?;
    let expression = match previous {
        Some(previous) => {
            OwnedScalarExpr::boolean([previous, expression].into_iter(), true, control)?
        }
        None => expression,
    };
    query.replace_where(Some(expression), control)
}

fn replace_source(
    mut source: MutableSourcePlan,
    qualifier: &str,
    replacement: MutableSourcePlan,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    control.check()?;
    match &*source {
        SourcePlan::Subquery {
            alias: Some(alias), ..
        } if alias == qualifier => {
            drop(source);
            Ok(replacement)
        }
        SourcePlan::Join { left, .. } => {
            let slot = if contains_qualifier(left, qualifier) {
                SourceChildSlot::Left
            } else {
                SourceChildSlot::Right
            };
            source = source.map_child(slot, control, |child| {
                replace_source(child, qualifier, replacement, control)
            })?;
            Ok(source)
        }
        _ => unreachable!("selected derived source remains in its input tree"),
    }
}

fn contains_qualifier(source: &SourcePlan, name: &str) -> bool {
    match source {
        SourcePlan::Join {
            alias: Some(alias), ..
        } => alias == name,
        SourcePlan::Join { left, right, .. } => {
            contains_qualifier(left, name) || contains_qualifier(right, name)
        }
        SourcePlan::Table {
            qualifier, alias, ..
        } => alias.as_deref().unwrap_or(qualifier) == name,
        SourcePlan::Values { alias, .. }
        | SourcePlan::Subquery { alias, .. }
        | SourcePlan::Function { alias, .. }
        | SourcePlan::FunctionGroup { alias, .. } => alias.as_deref() == Some(name),
        SourcePlan::RetainedRows(_) => false,
    }
}
