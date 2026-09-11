//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Candidate event catalogs and constraint identities for relation and dependency removal.
use super::{RuleCatalog, TriggerCatalog};
use crate::catalog::constraints::ConstraintIdentity;
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
pub struct RemovedRelationEvents {
    pub triggers: TriggerCatalog,
    pub rules: RuleCatalog,
    pub constraints: Vec<ConstraintIdentity>,
}
pub fn removed_relation_events(
    triggers: &TriggerCatalog,
    rules: &RuleCatalog,
    relation: &RelationIdentity,
) -> Result<Option<RemovedRelationEvents>, String> {
    let qualified = relation.qualified_name();
    let referenced_by_trigger = triggers.values().any(|entries| {
        entries.values().any(|trigger| {
            trigger.definition.referenced_table.as_deref() == Some(qualified.as_str())
        })
    });
    if !triggers.contains_key(relation) && !rules.contains_key(relation) && !referenced_by_trigger {
        return Ok(None);
    }
    let mut next_triggers = triggers.clone();
    let mut next_rules = rules.clone();
    let mut removed_constraint_identities = Vec::new();
    for (trigger_relation, entries) in triggers {
        for trigger in entries.values() {
            if trigger.definition.constraint
                && (trigger_relation == relation
                    || trigger.definition.referenced_table.as_deref() == Some(qualified.as_str()))
            {
                removed_constraint_identities.push(trigger.constraint_identity().map_err(
                    |error| format!("resolve dropped constraint-trigger identity: {error}"),
                )?);
            }
        }
    }
    next_triggers.remove(relation);
    for entries in next_triggers.values_mut() {
        entries.retain(|_, trigger| {
            trigger.definition.referenced_table.as_deref() != Some(qualified.as_str())
        });
    }
    next_triggers.retain(|_, entries| !entries.is_empty());
    next_rules.remove(relation);

    Ok(Some(RemovedRelationEvents {
        triggers: next_triggers,
        rules: next_rules,
        constraints: removed_constraint_identities,
    }))
}
pub fn removed_dependent_rules(
    rules: &RuleCatalog,
    dependents: &[(RelationIdentity, String)],
) -> Result<RuleCatalog, String> {
    let mut next = rules.clone();
    for (event_relation, name) in dependents {
        let removed = next
            .get_mut(event_relation)
            .and_then(|entries| entries.remove(name));
        if removed.is_none() {
            return Err(format!(
                "dependent rule `{name}` on `{}` disappeared after DROP preflight",
                event_relation.qualified_name()
            ));
        }
        if next.get(event_relation).is_some_and(BTreeMap::is_empty) {
            next.remove(event_relation);
        }
    }

    Ok(next)
}
