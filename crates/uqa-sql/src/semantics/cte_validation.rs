//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` statement ownership rules for data-modifying WITH definitions.

use super::rules::RuleCatalog;
use crate::catalog::resolution::RelationNameResolution;
use crate::plan::{
    CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use crate::SQLError;

pub struct CteValidationContext<'a> {
    pub catalog: &'a dyn RuleCatalog,
    pub resolution: &'a RelationNameResolution,
}

pub fn validate_plan(
    context: &CteValidationContext<'_>,
    plan: &UnifiedPlan,
) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => validate_query(context, query, true),
        UnifiedPlan::Command(command) => validate_command(context, command, true),
    }
}

fn contains_command(ctes: &[CtePlan]) -> bool {
    ctes.iter().any(|cte| cte.body.modifies_data())
}

fn validate_ctes(
    context: &CteValidationContext<'_>,
    ctes: &[CtePlan],
    top_level: bool,
) -> Result<(), SQLError> {
    if !top_level && contains_command(ctes) {
        return Err(SQLError::Unsupported(
            "WITH clause containing a data-modifying statement must be at the top level".into(),
        ));
    }
    for cte in ctes {
        if cte.recursive
            && cte.body.modifies_data()
            && crate::semantics::cte_references_own_name(cte)
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
            CtePlanBody::Query(query) => validate_query(context, query, false)?,
            CtePlanBody::Command(command) => {
                validate_command(context, command, false)?;
                validate_command_rules(context, command)?;
            }
        }
    }
    Ok(())
}

fn validate_query(
    context: &CteValidationContext<'_>,
    query: &QueryPlan,
    top_level: bool,
) -> Result<(), SQLError> {
    validate_ctes(context, &query.ctes, top_level)?;
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                validate_source(context, source)?;
            }
            for query in &block.subqueries {
                validate_query(context, query, false)?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            validate_query(context, left, false)?;
            validate_query(context, right, false)?;
            for query in subqueries {
                validate_query(context, query, false)?;
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for query in subqueries {
                validate_query(context, query, false)?;
            }
        }
    }
    Ok(())
}

fn validate_source(
    context: &CteValidationContext<'_>,
    source: &SourcePlan,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Subquery { body, .. } => validate_query(context, body, false),
        SourcePlan::Join { left, right, .. } => {
            validate_source(context, left)?;
            validate_source(context, right)
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => Ok(()),
    }
}

fn validate_command(
    context: &CteValidationContext<'_>,
    command: &CommandPlan,
    top_level: bool,
) -> Result<(), SQLError> {
    validate_ctes(context, command.ctes(), top_level)?;
    for query in command.query_inputs() {
        validate_query(context, query, false)?;
    }
    if let Some(source) = command.source_input() {
        validate_source(context, source)?;
    }
    match command {
        CommandPlan::CreateView { query, .. } => validate_query_owner(
            context,
            query,
            "views must not contain data-modifying statements in WITH",
        ),
        CommandPlan::CreateMaterializedView { query, .. } => validate_query_owner(
            context,
            query,
            "materialized views must not use data-modifying statements in WITH",
        ),
        CommandPlan::DeclareCursor { query, .. } => validate_query_owner(
            context,
            query,
            "DECLARE CURSOR must not contain data-modifying statements in WITH",
        ),
        CommandPlan::CreateTableAs { query, .. } => validate_query(context, query, true),
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            validate_plan(context, body)
        }
        _ => Ok(()),
    }
}

fn validate_query_owner(
    context: &CteValidationContext<'_>,
    query: &QueryPlan,
    message: &str,
) -> Result<(), SQLError> {
    validate_query(context, query, true)?;
    if contains_command(&query.ctes) {
        return Err(SQLError::Unsupported(message.into()));
    }
    Ok(())
}

fn validate_command_rules(
    context: &CteValidationContext<'_>,
    command: &CommandPlan,
) -> Result<(), SQLError> {
    use crate::ast::RuleEvent;
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
    if crate::catalog::resolve_virtual_relation(&context.resolution.search_path, table).is_some() {
        // Virtual catalog relations have no entries in the stored rewrite-rule registry.
        return Ok(());
    }
    let table = context.catalog.resolve_mutation_target(table, bound)?;
    let rules = context.catalog.rules_for(&table, event)?;
    if rules.is_empty() {
        return Ok(());
    }
    super::rules::validate_rule_returning_contract(
        context.catalog,
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
