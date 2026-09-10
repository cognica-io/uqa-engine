//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Surviving input dependencies across automatic views and rewrite rules.

use super::{
    automatic_view_layer, has_instead_of_trigger, BTreeSet, SQLError, TriggerEvent,
    ViewRewriteContext,
};
use crate::ast::RuleEvent;

pub struct RuleInputRequirements {
    pub columns: BTreeSet<String>,
    pub requires_rows: bool,
}

pub fn rule_input_requirements(
    services: ViewRewriteContext<'_>,
    table: &str,
    event: RuleEvent,
) -> Result<Option<RuleInputRequirements>, SQLError> {
    collect_requirements(services, table, event, &mut BTreeSet::new())
}

fn collect_requirements(
    services: ViewRewriteContext<'_>,
    table: &str,
    event: RuleEvent,
    visited: &mut BTreeSet<String>,
) -> Result<Option<RuleInputRequirements>, SQLError> {
    if !visited.insert(table.to_string()) {
        return Err(SQLError::Internal(format!(
            "cycle while resolving rewrite-rule inputs for `{table}`"
        )));
    }
    let rules = services.catalog.rules_for(table, event)?;
    let suppresses = rules
        .iter()
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none());
    let mut required = RuleInputRequirements {
        columns: BTreeSet::new(),
        requires_rows: rules
            .iter()
            .any(|rule| rule.definition.condition.is_some() || !rule.definition.actions.is_empty()),
    };
    for rule in &rules {
        let Some(columns) = services.catalog.rule_new_row_columns(rule)? else {
            return Ok(None);
        };
        required.columns.extend(columns);
    }
    if suppresses {
        return Ok(Some(required));
    }
    let trigger = match event {
        RuleEvent::Insert => TriggerEvent::Insert,
        RuleEvent::Update => TriggerEvent::Update,
        RuleEvent::Delete => TriggerEvent::Delete,
        RuleEvent::Select => return Ok(None),
    };
    if has_instead_of_trigger(services, table, trigger)? {
        return Ok(None);
    }
    let Some(layer) = automatic_view_layer(services, table)? else {
        return Ok(None);
    };
    let Some(source) = collect_requirements(services, &layer.source_name, event, visited)? else {
        return Ok(None);
    };
    required.requires_rows |= source.requires_rows;
    for column in layer.columns {
        if column
            .writable_source_column
            .as_ref()
            .is_some_and(|name| source.columns.contains(name))
        {
            required.columns.insert(column.name);
        }
    }
    Ok(Some(required))
}
