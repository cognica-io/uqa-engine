//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove inputs that do not survive rule rewriting before constant planning.

use crate::Engine;
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_execution::ScalarExpr;
use uqa_planner::{
    CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use uqa_sql::{ast::RuleEvent, SQLError};

pub(super) fn rewrite_plan(engine: &Engine, plan: &mut UnifiedPlan) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => rewrite_query(engine, query),
        UnifiedPlan::Command(command) => rewrite_command(engine, command),
    }
}

fn rewrite_ctes(engine: &Engine, ctes: &mut [CtePlan]) -> Result<(), SQLError> {
    for cte in ctes {
        match &mut cte.body {
            CtePlanBody::Query(query) => rewrite_query(engine, query)?,
            CtePlanBody::Command(command) => rewrite_command(engine, command)?,
        }
    }
    Ok(())
}

fn rewrite_query(engine: &Engine, query: &mut QueryPlan) -> Result<(), SQLError> {
    rewrite_ctes(engine, &mut query.ctes)?;
    let subqueries = match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                rewrite_source(engine, source)?;
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
            rewrite_query(engine, left)?;
            rewrite_query(engine, right)?;
            subqueries
        }
    };
    for query in subqueries {
        rewrite_query(engine, query)?;
    }
    Ok(())
}

fn rewrite_source(engine: &Engine, source: &mut SourcePlan) -> Result<(), SQLError> {
    match source {
        SourcePlan::Subquery { body, .. } => rewrite_query(engine, body)?,
        SourcePlan::Join { left, right, .. } => {
            rewrite_source(engine, left)?;
            rewrite_source(engine, right)?;
        }
        _ => {}
    }
    Ok(())
}

fn rewrite_command(engine: &Engine, command: &mut CommandPlan) -> Result<(), SQLError> {
    match command {
        CommandPlan::Explain { body, .. } => return rewrite_plan(engine, body),
        CommandPlan::DeclareCursor { query, .. } => return rewrite_query(engine, query),
        _ => {}
    }
    prune_rule_inputs(engine, command)?;
    if let Some(ctes) = command.ctes_mut() {
        rewrite_ctes(engine, ctes)?;
    }
    if let Some(source) = command.source_input_mut() {
        rewrite_source(engine, source)?;
    }
    for query in command.query_inputs_mut() {
        rewrite_query(engine, query)?;
    }
    Ok(())
}

fn prune_rule_inputs(engine: &Engine, command: &mut CommandPlan) -> Result<(), SQLError> {
    let (table, bound, event) = match command {
        CommandPlan::Insert(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Insert),
        CommandPlan::Update(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Update),
        CommandPlan::Delete(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Delete),
        _ => return Ok(()),
    };
    let table = engine.resolve_mutation_target_name(table, bound)?;
    let Some(requirements) = uqa_sql::semantics::view_rewrite::rule_input_requirements(
        engine.view_rewrite_context(),
        &table,
        event,
    )?
    else {
        return Ok(());
    };
    uqa_sql::semantics::rules::validate_rule_returning_contract(
        engine,
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
                crate::sql::query_source_column_names(engine, &table, true)?.unwrap_or_default()
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
                uqa_planner::mutation_outputs::prune_unused_query_outputs(
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
