//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Derived predicates retain admitted query constructors through specialization and optimization.

use super::{context::FilterPushdownContext, owned::PredicateServices};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    plan::{
        MutableQueryPlan, OwnedQueryPlan, OwnedScalarExpr, QueryChildSlot, QueryPlan,
        RelationalPlan,
    },
    semantics::{
        expr_contains_subquery, query_plan_output_columns_with_control,
        volatility::{
            expr_contains_volatile_function_with_control,
            query_contains_volatile_function_with_control,
        },
    },
    SQLError, ScalarExpr,
};

pub fn push_output_filter_into_query_plan_with_control(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    aliases: Option<&[String]>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedQueryPlan>, SQLError> {
    if query_contains_volatile_function_with_control(context.volatility, plan, control)? {
        return Ok(None);
    }
    push_derived_output_filter_with_control(context, plan, qualifier, filter, aliases, control)
}

pub fn push_derived_output_filter_with_control(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    aliases: Option<&[String]>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedQueryPlan>, SQLError> {
    let inferred;
    let columns = if let Some(schema) = plan
        .analyzed_output
        .as_ref()
        .filter(|schema| !schema.columns_are_open(None))
    {
        schema.columns()
    } else {
        inferred = query_plan_output_columns_with_control(plan, control)?;
        let Some(columns) = &inferred else {
            return Ok(None);
        };
        columns.as_slice()
    };
    if aliases.is_some_and(|aliases| aliases.len() > columns.len()) {
        return Ok(None);
    }
    let mut visible = ProductionVec::new(*control);
    for (index, column) in columns.iter().enumerate() {
        visible.push_produced(
            control.copy_text(
                aliases
                    .and_then(|aliases| aliases.get(index))
                    .unwrap_or(column),
            )?,
        )?;
    }
    if expr_contains_subquery(filter)
        || expr_contains_volatile_function_with_control(context.volatility, filter, control)?
        || !super::output_boundary::preserves_set_types_with_control(
            plan,
            qualifier,
            filter,
            Some(&visible),
            control,
        )?
    {
        return Ok(None);
    }
    let Some(specialized) = specialize_query_output_filter_with_control(
        context,
        plan,
        qualifier,
        filter,
        Some(&visible),
        control,
    )?
    else {
        return Ok(None);
    };
    (context.optimizer)(specialized, control).map(Some)
}

pub fn specialize_query_output_filter_with_control(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    aliases: Option<&[String]>,
    control: &ProductionControl<'_>,
) -> Result<Option<MutableQueryPlan>, SQLError> {
    let (query, selected) = specialize_owned(
        context,
        plan.copy_for_rewrite(control)?,
        qualifier,
        filter,
        aliases,
        control,
    )?;
    Ok(selected.then_some(query))
}

fn specialize_owned(
    context: FilterPushdownContext<'_>,
    mut query: MutableQueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    aliases: Option<&[String]>,
    control: &ProductionControl<'_>,
) -> Result<(MutableQueryPlan, bool), SQLError> {
    control.check()?;
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            let Some(rewritten) = super::owned::derived_output_predicate(
                PredicateServices {
                    routines: context.correlation.routines,
                    volatility: context.volatility,
                },
                block,
                qualifier,
                filter,
                aliases,
                false,
                control,
            )?
            else {
                return Ok((query, false));
            };
            let (query, existing) = query.take_where(control)?;
            let predicate = match existing {
                Some(existing) => {
                    OwnedScalarExpr::boolean([existing, rewritten].into_iter(), true, control)?
                }
                None => rewritten,
            };
            Ok((query.replace_where(Some(predicate), control)?, true))
        }
        RelationalPlan::SetOp {
            left,
            limit,
            offset,
            ..
        } => {
            if limit.is_some() || offset.is_some() {
                return Ok((query, false));
            }
            let inferred: Option<Produced<Vec<String>>>;
            let columns = if let Some(aliases) = aliases {
                aliases
            } else {
                inferred = query_plan_output_columns_with_control(left, control)?;
                let Some(columns) = &inferred else {
                    return Ok((query, false));
                };
                columns
            };
            for slot in [QueryChildSlot::Left, QueryChildSlot::Right] {
                let mut selected = false;
                query = query.map_query_child(slot, control, |child| {
                    let (child, valid) = specialize_owned(
                        context,
                        child,
                        qualifier,
                        filter,
                        Some(columns),
                        control,
                    )?;
                    selected = valid;
                    Ok(child)
                })?;
                if !selected {
                    return Ok((query, false));
                }
            }
            Ok((query, true))
        }
        RelationalPlan::Values { .. } => Ok((query, false)),
    }
}
