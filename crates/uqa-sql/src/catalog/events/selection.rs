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
) -> impl Iterator<Item = &'a StoredRule> {
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
    catalog.get(relation).is_some_and(|entries| {
        entries.values().any(|trigger| {
            trigger.definition.timing == timing
                && trigger.definition.row == row
                && trigger.definition.events.contains(&event)
        })
    })
}
