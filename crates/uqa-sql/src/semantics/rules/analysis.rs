//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored-rule lineage, column requirements, and event row types.
use super::{action_binding::RuleSourceCatalog, RuleCatalog};
use crate::semantics::returning::ReturningCatalog;
use crate::{ast::RuleEvent, SQLError};
use std::collections::BTreeSet;
#[derive(Clone, Copy)]
pub struct RuleAnalysisContext<'a> {
    pub rules: &'a dyn RuleCatalog,
    pub sources: &'a dyn RuleSourceCatalog,
    pub returning: &'a dyn ReturningCatalog,
    pub domains: &'a dyn crate::catalog::domain::DomainCatalog,
    pub composites: Option<&'a dyn crate::expr::composites::CompositeTypeCatalog>,
}
pub fn rule_condition_plan_references_row(rule: &crate::catalog::events::StoredRule) -> bool {
    rule.bound_condition_plan().is_some_and(|(plan, binding)| {
        super::action_binding::rule_condition_plan_references_whole_row(plan)
            || !super::action_binding::rule_condition_plan_row_columns(plan, binding).is_empty()
    })
}

pub fn relation_suppresses_original_query(
    context: RuleAnalysisContext<'_>,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    for rule in context.rules.capture_rules(table, event)?.iter() {
        let rule = rule?;
        if rule.definition.instead && rule.definition.condition.is_none() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn relation_rules_reference_row(
    context: RuleAnalysisContext<'_>,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    for rule in context.rules.capture_rules(table, event)?.iter() {
        let rule = rule?;
        let condition_references_row = if rule.bound_condition_plan().is_some() {
            rule_condition_plan_references_row(rule)
        } else {
            rule.definition
                .condition
                .as_ref()
                .is_some_and(super::action_binding::rule_expr_references_row)
        };
        if condition_references_row {
            return Ok(true);
        }
        for action in &rule.definition.actions {
            let target_columns =
                super::action_binding::rule_action_target_columns(context.sources, action)?;
            if super::action_binding::rule_statement_references_row(
                context.sources,
                action,
                &target_columns,
            )? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn relation_rules_require_event_rows(
    context: RuleAnalysisContext<'_>,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    for rule in context.rules.capture_rules(table, event)?.iter() {
        let rule = rule?;
        if rule.definition.condition.is_some() || !rule.definition.actions.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn surviving_view_rules_reference_row(
    context: RuleAnalysisContext<'_>,
    relations: &[String],
    event: RuleEvent,
) -> Result<bool, SQLError> {
    let mut references_row = false;
    for relation in relations {
        references_row |= relation_rules_reference_row(context, relation, event)?;
        if relation_suppresses_original_query(context, relation, event)? {
            break;
        }
    }
    Ok(references_row)
}

pub fn surviving_view_rules_require_event_rows(
    context: RuleAnalysisContext<'_>,
    relations: &[String],
    event: RuleEvent,
) -> Result<bool, SQLError> {
    let mut requires_rows = false;
    for relation in relations {
        requires_rows |= relation_rules_require_event_rows(context, relation, event)?;
        if relation_suppresses_original_query(context, relation, event)? {
            break;
        }
    }
    Ok(requires_rows)
}

pub fn relation_condition_row_columns(
    context: RuleAnalysisContext<'_>,
    table: &str,
    event: RuleEvent,
) -> Result<BTreeSet<String>, SQLError> {
    let mut columns = BTreeSet::new();
    for rule in context.rules.capture_rules(table, event)?.iter() {
        let rule = rule?;
        if let Some((plan, binding)) = rule.bound_condition_plan() {
            columns.extend(super::action_binding::rule_condition_plan_row_columns(
                plan, binding,
            ));
            continue;
        }
        if let Some(condition) = rule.definition.condition.as_ref() {
            columns.extend(super::action_binding::rule_expr_row_columns(condition));
        }
    }
    Ok(columns)
}

pub fn relation_rule_row_columns(
    context: RuleAnalysisContext<'_>,
    table: &str,
    event: RuleEvent,
) -> Result<Option<BTreeSet<String>>, SQLError> {
    Ok(relation_rule_row_columns_with_control(
        context,
        table,
        event,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .map(|columns| {
        columns
            .into_uncontrolled()
            .expect("ordinary rule row columns")
            .into_iter()
            .collect()
    }))
}

pub use super::action_binding::relation_rule_row_columns_with_control;

pub fn rule_returning_columns(
    context: RuleAnalysisContext<'_>,
    table: &str,
) -> Result<Vec<crate::ast::ColumnDef>, SQLError> {
    let columns = context
        .returning
        .try_describe_table_row_type(table)
        .map_err(|error| SQLError::Internal(format!("read rule RETURNING row type: {error}")))?;
    if let Some(columns) = columns {
        return Ok(columns);
    }
    context.sources.rule_relation_definitions(table)
}

pub fn rule_returning_columns_with_control(
    context: RuleAnalysisContext<'_>,
    table: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<Vec<crate::ast::ColumnDef>>, SQLError> {
    control.check()?;
    let mut output = uqa_core::memory::ProductionVec::new(*control);
    let found = context
        .returning
        .with_table_row_type_with_control(table, control, &mut |columns| {
            for column in columns {
                output.push_produced(crate::plan::copy::copy_syntax_with_control(
                    column, control,
                )?)?;
            }
            Ok(())
        })
        .map_err(|error| match error {
            SQLError::Internal(message) => {
                match control.format(format_args!("read rule RETURNING row type: {message}")) {
                    Ok(message) => SQLError::Internal(message.into_parts().0),
                    Err(error) => error.into(),
                }
            }
            other => other,
        })?;
    if found {
        return Ok(output.finish()?);
    }
    context
        .sources
        .rule_relation_definitions_with_control(table, control)
}
