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
}
pub fn rewrite_plan(
    context: &RuleInputPlanningContext<'_>,
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => rewrite_query(context, query),
        UnifiedPlan::Command(command) => rewrite_command(context, command),
    }
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
        CommandPlan::Explain { body, .. } => return rewrite_plan(context, body),
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
                plan.columns.clone()
            };
            let positions = columns
                .iter()
                .enumerate()
                .filter_map(|(position, column)| required.contains(column).then_some(position))
                .collect::<BTreeSet<_>>();
            for row in &mut plan.rows {
                for (position, expression) in row.iter_mut().enumerate() {
                    if !positions.contains(&position) {
                        *expression = ScalarExpr::Literal(Value::Null);
                    }
                }
            }
            if !requires_rows {
                plan.source = None;
                plan.ctes.clear();
                plan.subqueries.clear();
            } else if let Some(source) = &mut plan.source {
                crate::mutation_outputs::prune_unused_query_outputs(
                    source,
                    &positions,
                    columns.len(),
                );
            }
        }
        CommandPlan::Update(plan) => {
            for assignment in &mut plan.assignments {
                if !required.contains(&assignment.column) {
                    assignment.value = ScalarExpr::Literal(Value::Null);
                }
            }
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
