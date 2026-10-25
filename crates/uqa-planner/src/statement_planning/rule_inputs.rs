//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prune mutation inputs discarded by rewrite rules before constant evaluation.
use crate::{
    CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::{
    ast::RuleEvent,
    semantics::{rules::RuleCatalog, view_rewrite::context::ViewRewriteContext},
    SQLError, ScalarExpr,
};
mod view_projection;

pub trait RuleInputColumns {
    fn source_column_names(
        &self,
        table: &str,
        relations_bound: bool,
    ) -> Result<Option<Vec<String>>, SQLError>;
}
pub struct RuleInputPlanningContext<'a> {
    pub rules: &'a dyn RuleCatalog,
    pub views: ViewRewriteContext<'a>,
    pub columns: &'a dyn RuleInputColumns,
    pub volatility: &'a dyn uqa_sql::semantics::volatility::VolatilityCatalog,
}
pub fn rewrite_plan(
    context: &RuleInputPlanningContext<'_>,
    plan: &mut UnifiedPlan,
    config: &crate::optimizer::OptimizerConfig<'_>,
) -> Result<(), SQLError> {
    view_projection::rewrite(context, plan, config)?;
    prune_captured_inputs(context.volatility, plan);
    match plan {
        UnifiedPlan::Query(query) => rewrite_query(context, query),
        UnifiedPlan::Command(command) => rewrite_command(context, command),
    }
}

/// Internal producer slots have structural identities. Their unused projections can be removed before expression folding without changing the query's cardinality or visible namespace.
fn prune_captured_inputs(
    volatility: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    plan: &mut UnifiedPlan,
) {
    let mut required = std::collections::BTreeMap::<_, BTreeSet<usize>>::new();
    plan.visit_scalar_expressions(&mut |expression| {
        expression.visit(&mut |node| {
            let column = match node {
                ScalarExpr::InternalColumn(column) => Some(*column),
                ScalarExpr::Func {
                    binding: Some(binding),
                    ..
                } => uqa_sql::semantics::function_implicit_row_input(binding),
                _ => None,
            };
            if let Some(column) = column {
                required
                    .entry(column.relation())
                    .or_default()
                    .insert(column.attribute());
            }
        });
    });
    let empty = BTreeSet::new();
    plan.visit_sources_mut(&mut |source| {
        if let SourcePlan::Subquery {
            captured_output: Some(relation),
            body,
            ..
        } = source
        {
            if let RelationalPlan::QueryBlock(block) = &body.root {
                let width = block.projections.len();
                crate::mutation_outputs::prune_unused_query_outputs(
                    volatility,
                    body,
                    required.get(relation).unwrap_or(&empty),
                    width,
                );
            }
        }
    });
}

fn rewrite_ctes(
    context: &RuleInputPlanningContext<'_>,
    ctes: &mut [CtePlan],
) -> Result<(), SQLError> {
    for cte in ctes {
        match &mut cte.body {
            CtePlanBody::Query(query) => rewrite_query(context, query)?,
            CtePlanBody::Command(command) => rewrite_command(context, command)?,
        }
    }
    Ok(())
}

fn rewrite_query(
    context: &RuleInputPlanningContext<'_>,
    query: &mut QueryPlan,
) -> Result<(), SQLError> {
    rewrite_ctes(context, &mut query.ctes)?;
    let subqueries = match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                rewrite_source(context, source)?;
            }
            &mut block.subqueries
        }
        RelationalPlan::Values { subqueries, .. } => subqueries,
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            rewrite_query(context, left)?;
            rewrite_query(context, right)?;
            subqueries
        }
    };
    for query in subqueries {
        rewrite_query(context, query)?;
    }
    Ok(())
}

fn rewrite_source(
    context: &RuleInputPlanningContext<'_>,
    source: &mut SourcePlan,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Subquery { body, .. } => rewrite_query(context, body)?,
        SourcePlan::Join { left, right, .. } => {
            rewrite_source(context, left)?;
            rewrite_source(context, right)?;
        }
        _ => {}
    }
    Ok(())
}

fn rewrite_command(
    context: &RuleInputPlanningContext<'_>,
    command: &mut CommandPlan,
) -> Result<(), SQLError> {
    match command {
        CommandPlan::Explain { .. } => return Ok(()),
        CommandPlan::DeclareCursor { query, .. } => return rewrite_query(context, query),
        _ => {}
    }
    prune_rule_inputs(context, command)?;
    if let Some(ctes) = command.ctes_mut() {
        rewrite_ctes(context, ctes)?;
    }
    if let Some(source) = command.source_input_mut() {
        rewrite_source(context, source)?;
    }
    for query in command.query_inputs_mut() {
        rewrite_query(context, query)?;
    }
    Ok(())
}

fn prune_rule_inputs(
    context: &RuleInputPlanningContext<'_>,
    command: &mut CommandPlan,
) -> Result<(), SQLError> {
    let (table, bound, event) = match command {
        CommandPlan::Insert(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Insert),
        CommandPlan::Update(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Update),
        CommandPlan::Delete(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Delete),
        _ => return Ok(()),
    };
    let table = context.rules.resolve_mutation_target(table, bound)?;
    validate_rule_contracts(context, command, &table, event)?;
    if let CommandPlan::Update(plan) = command {
        uqa_sql::semantics::view_rewrite::validate_grouped_rule_inputs(
            context.views,
            &table,
            &plan.assignments,
        )?;
    }
    let Some(requirements) =
        uqa_sql::semantics::view_rewrite::rule_input_requirements(context.views, &table, event)?
    else {
        return Ok(());
    };
    uqa_sql::semantics::rules::validate_rule_returning_contract(
        context.rules,
        &table,
        event,
        command
            .returning()
            .is_some_and(|returning| !returning.is_empty()),
    )?;
    let requires_rows = requirements.requires_rows;
    let required = requirements.columns;
    match command {
        CommandPlan::Insert(plan) => {
            let columns = if plan.columns.is_empty() {
                context
                    .columns
                    .source_column_names(&table, true)?
                    .unwrap_or_default()
            } else {
                plan.columns
                    .iter()
                    .map(|target| target.column.clone())
                    .collect()
            };
            let positions = columns
                .iter()
                .enumerate()
                .filter_map(|(position, column)| required.contains(column).then_some(position))
                .collect::<BTreeSet<_>>();
            for (position, target) in plan.columns.iter_mut().enumerate() {
                if !positions.contains(&position) {
                    for expression in target.expressions_mut() {
                        *expression = ScalarExpr::Literal(Value::Null);
                    }
                }
            }
            if plan.rows.len() == 1 || !requires_rows {
                for row in &mut plan.rows {
                    for (position, expression) in row.iter_mut().enumerate() {
                        if !positions.contains(&position) {
                            *expression = ScalarExpr::Literal(Value::Null);
                        }
                    }
                }
            }
            if !requires_rows {
                plan.source = None;
                plan.ctes.clear();
                plan.subqueries.clear();
            } else if let Some(source) = &mut plan.source {
                crate::mutation_outputs::prune_unused_query_outputs(
                    context.volatility,
                    source,
                    &positions,
                    columns.len(),
                );
            }
        }
        CommandPlan::Update(plan) => {
            prune_update_assignments(&mut plan.assignments, &required);
            if !requires_rows {
                plan.source = None;
                plan.predicate = None;
                plan.ctes.clear();
                plan.subqueries.clear();
            }
        }
        CommandPlan::Delete(plan) if !requires_rows => {
            plan.source = None;
            plan.predicate = None;
            plan.ctes.clear();
            plan.subqueries.clear();
        }
        _ => {}
    }
    Ok(())
}

fn prune_update_assignments(
    assignments: &mut Vec<uqa_sql::plan::AssignmentPlan>,
    required: &BTreeSet<String>,
) {
    *assignments = std::mem::take(assignments)
        .into_iter()
        .flat_map(|assignment| {
            if assignment
                .target
                .column_names()
                .all(|column| !required.contains(column))
            {
                assignment
                    .target
                    .targets()
                    .iter()
                    .cloned()
                    .map(|target| uqa_sql::plan::AssignmentPlan {
                        target: target.into(),
                        value: ScalarExpr::Literal(Value::Null),
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![assignment]
            }
        })
        .collect();
    for assignment in assignments {
        if assignment
            .target
            .column_names()
            .all(|column| !required.contains(column))
        {
            for expression in assignment.expressions_mut() {
                *expression = ScalarExpr::Literal(Value::Null);
            }
        }
    }
}

fn validate_rule_contracts(
    context: &RuleInputPlanningContext<'_>,
    command: &CommandPlan,
    table: &str,
    event: RuleEvent,
) -> Result<(), SQLError> {
    if !command.ctes().is_empty() {
        uqa_sql::semantics::rules::validate_rule_cte_contract(
            context.rules,
            table,
            event,
            |table, event| {
                uqa_sql::semantics::view_rewrite::rule_inputs::original_rule_target(
                    context.views,
                    table,
                    event,
                )
            },
        )?;
    }
    if let CommandPlan::Insert(insert) = command {
        uqa_sql::semantics::rules::validate_insert_rule_query_contract(
            context.rules,
            table,
            insert,
            |table, event| {
                uqa_sql::semantics::view_rewrite::rule_inputs::original_rule_target(
                    context.views,
                    table,
                    event,
                )
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
