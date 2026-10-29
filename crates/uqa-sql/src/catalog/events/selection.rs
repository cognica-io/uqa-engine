//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule and trigger predicates shared by live execution and retained metadata inquiries.

use super::{RuleCatalog, StoredRule, TriggerCatalog};
use crate::ast::{RuleEvent, TriggerEvent, TriggerTiming};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;

fn matching_rules<'a>(
    catalog: &'a RuleCatalog,
    relation: &RelationIdentity,
    event: RuleEvent,
) -> impl DoubleEndedIterator<Item = &'a StoredRule> {
    catalog
        .get(relation)
        .into_iter()
        .flat_map(BTreeMap::values)
        .filter(move |rule| rule.definition.event == event)
}

pub fn rule_definitions(
    catalog: &RuleCatalog,
    relation: &RelationIdentity,
    event: RuleEvent,
) -> Vec<StoredRule> {
    matching_rules(catalog, relation, event).cloned().collect()
}

pub fn active_rules(
    catalog: &RuleCatalog,
    relation: &RelationIdentity,
    event: RuleEvent,
    replica: bool,
) -> Vec<StoredRule> {
    matching_rules(catalog, relation, event)
        .filter(|rule| {
            if replica {
                rule.enabled.fires_in_replica()
            } else {
                rule.enabled.fires_in_origin()
            }
        })
        .cloned()
        .collect()
}

pub fn has_trigger_definition(
    catalog: &TriggerCatalog,
    relation: &RelationIdentity,
    timing: TriggerTiming,
    event: TriggerEvent,
    row: bool,
) -> bool {
    has_trigger_definition_with_control(
        catalog,
        relation,
        timing,
        event,
        row,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary trigger selection")
}

pub fn has_trigger_definition_with_control(
    catalog: &TriggerCatalog,
    relation: &RelationIdentity,
    timing: TriggerTiming,
    event: TriggerEvent,
    row: bool,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, crate::SQLError> {
    control.check()?;
    for trigger in catalog.get(relation).into_iter().flat_map(BTreeMap::values) {
        control.check()?;
        if trigger.definition.timing == timing
            && trigger.definition.row == row
            && trigger.definition.events.contains(&event)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Borrow selected rules in the same catalog order without copying executable declarations.
pub fn visit_active_rules_with_control(
    catalog: &RuleCatalog,
    relation: &RelationIdentity,
    event: RuleEvent,
    replica: bool,
    reverse: bool,
    control: &uqa_core::memory::ProductionControl<'_>,
    visit: &mut dyn FnMut(&StoredRule) -> Result<(), crate::SQLError>,
) -> Result<(), crate::SQLError> {
    control.check()?;
    let rules = catalog.get(relation).into_iter().flat_map(BTreeMap::values);
    let mut selected = |rule: &StoredRule| {
        control.check()?;
        let enabled = if replica {
            rule.enabled.fires_in_replica()
        } else {
            rule.enabled.fires_in_origin()
        };
        if rule.definition.event == event && enabled {
            visit(rule)?;
        }
        Ok::<_, crate::SQLError>(())
    };
    if reverse {
        for rule in rules.rev() {
            selected(rule)?;
        }
    } else {
        for rule in rules {
            selected(rule)?;
        }
    }
    Ok(())
}
