//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored-rule lineage, column requirements, and event row types.
use super::{action_binding::RuleSourceCatalog, binding::RuleColumnMetadata, RuleCatalog};
use crate::semantics::returning::ReturningCatalog;
use crate::{ast::RuleEvent, SQLError};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Copy)]
pub struct RuleAnalysisContext<'a> {
    pub rules: &'a dyn RuleCatalog,
    pub sources: &'a dyn RuleSourceCatalog,
    pub returning: &'a dyn ReturningCatalog,
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
    Ok(context
        .rules
        .rules_for(table, event)?
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none()))
}

pub fn relation_rules_reference_row(
    context: RuleAnalysisContext<'_>,
    table: &str,
    event: RuleEvent,
) -> Result<bool, SQLError> {
    for rule in context.rules.rules_for(table, event)? {
        let condition_references_row = if rule.bound_condition_plan().is_some() {
            rule_condition_plan_references_row(&rule)
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
    Ok(context
        .rules
        .rules_for(table, event)?
        .iter()
        .any(|rule| rule.definition.condition.is_some() || !rule.definition.actions.is_empty()))
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
    for rule in context.rules.rules_for(table, event)? {
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
    let mut columns = BTreeSet::new();
    let mut references_row = false;
    let mut references_whole_row = false;
    for rule in context.rules.rules_for(table, event)? {
        if let Some((plan, binding)) = rule.bound_condition_plan() {
            let plan_references_row = rule_condition_plan_references_row(&rule);
            references_row |= plan_references_row;
            references_whole_row |=
                super::action_binding::rule_condition_plan_references_whole_row(plan);
            columns.extend(super::action_binding::rule_condition_plan_row_columns(
                plan, binding,
            ));
        } else if let Some(condition) = rule.definition.condition.as_ref() {
            references_row |= super::action_binding::rule_expr_references_row(condition);
            references_whole_row |=
                super::action_binding::rule_expr_references_whole_row(condition);
            columns.extend(super::action_binding::rule_expr_row_columns(condition));
        }
        for action in &rule.definition.actions {
            let action_columns =
                super::action_binding::rule_action_target_columns(context.sources, action)?;
            references_row |= super::action_binding::rule_statement_references_row(
                context.sources,
                action,
                &action_columns,
            )?;
            references_whole_row |= super::action_binding::rule_statement_references_whole_row(
                context.sources,
                action,
                &action_columns,
            )?;
            columns.extend(super::action_binding::rule_statement_row_columns(
                context.sources,
                action,
                &action_columns,
            )?);
        }
    }
    if references_whole_row || references_row && columns.is_empty() {
        Ok(None)
    } else {
        Ok(Some(columns))
    }
}

pub fn rule_columns(
    context: RuleAnalysisContext<'_>,
    table: &str,
) -> Result<BTreeMap<String, RuleColumnMetadata>, SQLError> {
    let columns = context
        .returning
        .try_describe_table_row_type(table)
        .map_err(|error| SQLError::Internal(format!("read rule row type: {error}")))?;
    if let Some(columns) = columns {
        return Ok(columns
            .into_iter()
            .enumerate()
            .map(|(position, column)| {
                let uses_document_id = column.primary_key && column.ty.is_integer();
                (
                    column.name,
                    RuleColumnMetadata {
                        ty: column.ty,
                        collation_oid: column.collation_oid,
                        uses_document_id,
                        position,
                    },
                )
            })
            .collect());
    }
    Ok(context
        .sources
        .rule_relation_definitions(table)?
        .into_iter()
        .enumerate()
        .map(|(position, column)| {
            (
                column.name,
                RuleColumnMetadata {
                    ty: column.ty,
                    collation_oid: column.collation_oid,
                    uses_document_id: false,
                    position,
                },
            )
        })
        .collect())
}

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
