//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Candidate event catalogs and constraint identities for relation and dependency removal.
use super::{RuleCatalog, TriggerCatalog};
use crate::catalog::constraints::ConstraintIdentity;
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
