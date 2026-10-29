//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule dependency collection retains admitted names and nodes across automatic view layers.

use super::super::{context::ViewRewriteContext, production::automatic_view_layer_owned};
use crate::{
    ast::{RuleEvent, TriggerEvent, TriggerTiming},
    SQLError,
};
use uqa_core::memory::{OwnedMap, Produced, ProductionControl};

type Names = OwnedMap<Produced<String>, ()>;

pub struct OwnedRuleInputRequirements {
    pub columns: Names,
    pub requires_rows: bool,
}

fn insert(
    names: &mut Names,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if names.contains_key(name) {
        return Ok(false);
    }
    let name = control.copy_text(name)?;
    if let Some(budget) = control.budget() {
        names.insert_budgeted(name, (), budget)?;
    } else {
        names.insert(name, ());
    }
    Ok(true)
}

pub fn has_instead_of_trigger_with_control(
    services: ViewRewriteContext<'_>,
    table: &str,
    event: TriggerEvent,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let Some(canonical) = services
        .catalog
        .try_resolve_view_name_with_control(table, control)
        .map_err(|error| {
            if matches!(error.sqlstate(), Some("53200" | "57014")) {
                error
            } else {
                SQLError::Internal(format!("resolve DML view `{table}`: {error}"))
            }
        })?
    else {
        return Ok(false);
    };
    services.catalog.has_trigger_definition_with_control(
        &canonical,
        TriggerTiming::InsteadOf,
        event,
        true,
        control,
    )
}

fn trigger(event: RuleEvent) -> Option<TriggerEvent> {
    match event {
        RuleEvent::Insert => Some(TriggerEvent::Insert),
        RuleEvent::Update => Some(TriggerEvent::Update),
        RuleEvent::Delete => Some(TriggerEvent::Delete),
        RuleEvent::Select => None,
    }
}

pub fn original_rule_target_with_control(
    services: ViewRewriteContext<'_>,
    table: &str,
    event: RuleEvent,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>, SQLError> {
    let Some(event) = trigger(event) else {
        return Ok(None);
    };
    if has_instead_of_trigger_with_control(services, table, event, control)? {
        return Ok(None);
    }
    Ok(automatic_view_layer_owned(services, table, control)?.map(|layer| layer.source_name))
}

pub fn rule_input_requirements_with_control(
    services: ViewRewriteContext<'_>,
    table: &str,
    event: RuleEvent,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedRuleInputRequirements>, SQLError> {
    collect(services, table, event, &mut Names::new(), control)
}

fn collect(
    services: ViewRewriteContext<'_>,
    table: &str,
    event: RuleEvent,
    visited: &mut Names,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedRuleInputRequirements>, SQLError> {
    if !insert(visited, table, control)? {
        return Err(SQLError::Internal(format!(
            "cycle while resolving rewrite-rule inputs for `{table}`"
        )));
    }
    let mut required = OwnedRuleInputRequirements {
        columns: Names::new(),
        requires_rows: false,
    };
    let mut suppresses = false;
    let mut unknown = false;
    services
        .catalog
        .visit_rules_with_control(table, event, false, control, &mut |rule| {
            if unknown {
                return Ok(());
            }
            suppresses |= rule.definition.instead && rule.definition.condition.is_none();
            required.requires_rows |=
                rule.definition.condition.is_some() || !rule.definition.actions.is_empty();
            match services
                .catalog
                .rule_new_row_columns_with_control(rule, control)?
            {
                Some(columns) => {
                    for column in columns.iter() {
                        insert(&mut required.columns, column, control)?;
                    }
                }
                None => unknown = true,
            }
            Ok(())
        })?;
    if unknown {
        return Ok(None);
    }
    if suppresses {
        return Ok(Some(required));
    }
    let Some(trigger) = trigger(event) else {
        return Ok(None);
    };
    if has_instead_of_trigger_with_control(services, table, trigger, control)? {
        return Ok(None);
    }
    let Some(layer) = automatic_view_layer_owned(services, table, control)? else {
        return Ok(None);
    };
    let Some(source) = collect(services, &layer.source_name, event, visited, control)? else {
        return Ok(None);
    };
    required.requires_rows |= source.requires_rows;
    for column in layer.columns.iter() {
        control.check()?;
        if column
            .writable_source_column
            .as_ref()
            .is_some_and(|name| source.columns.contains_key(name.as_str()))
        {
            insert(&mut required.columns, &column.name, control)?;
        }
    }
    Ok(Some(required))
}

pub fn validate_grouped_rule_inputs_with_control(
    services: ViewRewriteContext<'_>,
    table: &str,
    assignments: &[crate::plan::AssignmentPlan],
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut columns = Names::new();
    for assignment in assignments {
        control.check()?;
        if matches!(
            assignment.target,
            crate::ast::AssignmentTargets::Multiple(_)
        ) {
            for name in assignment.target.column_names() {
                insert(&mut columns, name, control)?;
            }
        }
    }
    let mut table = control.copy_text(table)?;
    let mut visited = Names::new();
    while !columns.is_empty() && insert(&mut visited, &table, control)? {
        let mut suppresses = false;
        services.catalog.visit_rules_with_control(&table, RuleEvent::Update, false, control, &mut |rule| {
            if services.catalog.rule_new_row_columns_with_control(rule, control)?.is_none_or(|new| new.iter().any(|name| columns.contains_key(name.as_str()))) {
                return Err(SQLError::Routine { sqlstate: "0A000".into(), message: "NEW variables in ON UPDATE rules cannot reference columns that are part of a multiple assignment in the subject UPDATE command".into() });
            }
            suppresses |= rule.definition.instead && rule.definition.condition.is_none();
            Ok(())
        })?;
        if suppresses
            || has_instead_of_trigger_with_control(services, &table, TriggerEvent::Update, control)?
        {
            break;
        }
        let Some(layer) = automatic_view_layer_owned(services, &table, control)? else {
            break;
        };
        let mut next = Names::new();
        for column in layer.columns.iter() {
            control.check()?;
            if columns.contains_key(column.name.as_str()) {
                if let Some(name) = &column.writable_source_column {
                    insert(&mut next, name, control)?;
                }
            }
        }
        columns = next;
        table = layer.source_name;
    }
    Ok(())
}
