//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` statement ownership rules for data-modifying WITH definitions.

use crate::Engine;
use uqa_planner::{
    CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use uqa_sql::SQLError;

pub(super) fn validate_plan(engine: &Engine, plan: &UnifiedPlan) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => validate_query(engine, query, true),
        UnifiedPlan::Command(command) => validate_command(engine, command, true),
    }
}

fn contains_command(ctes: &[CtePlan]) -> bool {
    ctes.iter().any(|cte| cte.body.modifies_data())
}

fn validate_ctes(engine: &Engine, ctes: &[CtePlan], top_level: bool) -> Result<(), SQLError> {
    if !top_level && contains_command(ctes) {
        return Err(SQLError::Unsupported(
            "WITH clause containing a data-modifying statement must be at the top level".into(),
        ));
    }
    for cte in ctes {
        if cte.recursive && cte.body.modifies_data() && super::select::cte_references_own_name(cte)
        {
            return Err(SQLError::Routine {
                sqlstate: "42P19".into(),
                message: format!(
                    "recursive query \"{}\" must not contain data-modifying statements",
                    cte.name
                ),
            });
        }
        match &cte.body {
            CtePlanBody::Query(query) => validate_query(engine, query, false)?,
            CtePlanBody::Command(command) => {
                validate_command(engine, command, false)?;
                validate_command_rules(engine, command)?;
            }
        }
    }
    Ok(())
}

fn validate_query(engine: &Engine, query: &QueryPlan, top_level: bool) -> Result<(), SQLError> {
    validate_ctes(engine, &query.ctes, top_level)?;
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                validate_source(engine, source)?;
            }
            for query in &block.subqueries {
                validate_query(engine, query, false)?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            validate_query(engine, left, false)?;
            validate_query(engine, right, false)?;
            for query in subqueries {
                validate_query(engine, query, false)?;
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for query in subqueries {
                validate_query(engine, query, false)?;
            }
        }
    }
    Ok(())
}

fn validate_source(engine: &Engine, source: &SourcePlan) -> Result<(), SQLError> {
    match source {
        SourcePlan::Subquery { body, .. } => validate_query(engine, body, false),
        SourcePlan::Join { left, right, .. } => {
            validate_source(engine, left)?;
            validate_source(engine, right)
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => Ok(()),
    }
}

fn validate_command(
    engine: &Engine,
    command: &CommandPlan,
    top_level: bool,
) -> Result<(), SQLError> {
    validate_ctes(engine, command.ctes(), top_level)?;
    for query in command.query_inputs() {
        validate_query(engine, query, false)?;
    }
    if let Some(source) = command.source_input() {
        validate_source(engine, source)?;
    }
    match command {
        CommandPlan::CreateView { query, .. } => validate_query_owner(
            engine,
            query,
            "views must not contain data-modifying statements in WITH",
        ),
        CommandPlan::CreateMaterializedView { query, .. } => validate_query_owner(
            engine,
            query,
            "materialized views must not use data-modifying statements in WITH",
        ),
        CommandPlan::DeclareCursor { query, .. } => validate_query_owner(
            engine,
            query,
            "DECLARE CURSOR must not contain data-modifying statements in WITH",
        ),
        CommandPlan::CreateTableAs { query, .. } => validate_query(engine, query, true),
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            validate_plan(engine, body)
        }
        _ => Ok(()),
    }
}

fn validate_query_owner(engine: &Engine, query: &QueryPlan, message: &str) -> Result<(), SQLError> {
    validate_query(engine, query, true)?;
    if contains_command(&query.ctes) {
        return Err(SQLError::Unsupported(message.into()));
    }
    Ok(())
}

fn validate_command_rules(engine: &Engine, command: &CommandPlan) -> Result<(), SQLError> {
    use uqa_sql::ast::RuleEvent;
    let (table, bound, event) = match command {
        CommandPlan::Insert(plan) => (
            plan.table.as_str(),
            plan.target_relation_bound,
            RuleEvent::Insert,
        ),
        CommandPlan::Update(plan) => (
            plan.table.as_str(),
            plan.target_relation_bound,
            RuleEvent::Update,
        ),
        CommandPlan::Delete(plan) => (
            plan.table.as_str(),
            plan.target_relation_bound,
            RuleEvent::Delete,
        ),
        _ => return Ok(()),
    };
    if super::catalog::is_virtual_catalog_relation(
        &engine.session_execution_view().relation_name_resolution(),
        table,
    ) {
        // Virtual catalog relations have no entries in the stored rewrite-rule registry.
        return Ok(());
    }
    let table = super::dml::resolve_dml_target_name(engine, table, bound)?;
    let rules = engine.rules_for(&table, event)?;
    if rules.is_empty() {
        return Ok(());
    }
    super::rules::validate_rule_returning_contract(
        engine,
        &table,
        event,
        command
            .returning()
            .is_some_and(|returning| !returning.is_empty()),
    )?;
    for rule in rules {
        let rule = &rule.definition;
        let kind = if !rule.instead && !rule.actions.is_empty() {
            Some("DO ALSO")
        } else if rule.instead && rule.condition.is_some() {
            Some("conditional DO INSTEAD")
        } else if rule.instead && rule.actions.is_empty() {
            Some("DO INSTEAD NOTHING")
        } else if rule.instead && rule.actions.len() > 1 {
            Some("multi-statement DO INSTEAD")
        } else {
            None
        };
        if let Some(kind) = kind {
            return Err(SQLError::Unsupported(format!(
                "{kind} rules are not supported for data-modifying statements in WITH"
            )));
        }
    }
    Ok(())
}
