//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Surviving input dependencies across automatic views and rewrite rules.

use super::{
    automatic_view_layer, has_instead_of_trigger, BTreeSet, Engine, SQLError, TriggerEvent,
};
use uqa_sql::ast::RuleEvent;

pub(in crate::sql) struct RuleInputRequirements {
    pub(in crate::sql) columns: BTreeSet<String>,
    pub(in crate::sql) requires_rows: bool,
}

pub(in crate::sql) fn rule_input_requirements(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<Option<RuleInputRequirements>, SQLError> {
    collect_requirements(engine, table, event, &mut BTreeSet::new())
}

fn collect_requirements(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
    visited: &mut BTreeSet<String>,
) -> Result<Option<RuleInputRequirements>, SQLError> {
    if !visited.insert(table.to_string()) {
        return Err(SQLError::Internal(format!(
            "cycle while resolving rewrite-rule inputs for `{table}`"
        )));
    }
    let rules = engine.rules_for(table, event)?;
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
        let Some(columns) = crate::engine_events::rule_new_row_columns(engine, rule)? else {
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
    if has_instead_of_trigger(engine, table, trigger)? {
        return Ok(None);
    }
    let Some(layer) = automatic_view_layer(engine, table)? else {
        return Ok(None);
    };
    let Some(source) = collect_requirements(engine, &layer.source_name, event, visited)? else {
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
