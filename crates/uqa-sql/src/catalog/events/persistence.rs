//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable event-catalog envelopes and persistence selection over live relation metadata.
use super::{RuleCatalog, StoredRule, StoredTrigger, TriggerCatalog};
use crate::ast::RelationPersistence;
use serde::{Deserialize, Serialize};
use uqa_core::RelationIdentity;
pub const RULE_CATALOG_FORMAT_VERSION: u32 = 3;
pub trait EventRelationPersistence {
    fn rule_relation_is_temporary(&self, relation: &RelationIdentity) -> bool;
    fn trigger_relation_persistence(
        &self,
        relation: &RelationIdentity,
    ) -> Option<RelationPersistence>;
}
#[derive(Default, Serialize, Deserialize)]
pub struct StoredTriggerCatalog {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<StoredTrigger>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct StoredRuleCatalog {
    #[serde(default)]
    pub format_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<StoredRule>,
}

pub fn stored_rules_snapshot(
    rules: &RuleCatalog,
    relations: &dyn EventRelationPersistence,
) -> StoredRuleCatalog {
    StoredRuleCatalog {
        format_version: RULE_CATALOG_FORMAT_VERSION,
        rules: rules
            .iter()
            .filter(|(relation, _)| !relations.rule_relation_is_temporary(relation))
            .flat_map(|(_, entries)| entries.values().cloned())
            .collect(),
    }
}

pub fn stored_triggers_snapshot(
    triggers: &TriggerCatalog,
    relations: &dyn EventRelationPersistence,
) -> StoredTriggerCatalog {
    StoredTriggerCatalog {
        triggers: triggers
            .iter()
            .filter(|(relation, _)| {
                relations
                    .trigger_relation_persistence(relation)
                    .is_some_and(|persistence| persistence != RelationPersistence::Temporary)
            })
            .flat_map(|(_, entries)| entries.values().cloned())
            .collect(),
    }
}

pub fn rule_catalog_requires_migration(
    format_version: u32,
    allows_migration: bool,
) -> Result<bool, String> {
    if format_version > RULE_CATALOG_FORMAT_VERSION {
        return Err(format!(
            "rule catalog format {format_version} is newer than supported format {RULE_CATALOG_FORMAT_VERSION}"
        ));
    }
    let migrating_catalog = format_version < RULE_CATALOG_FORMAT_VERSION;
    if migrating_catalog && !allows_migration {
        return Err("rule catalog requires an initial-open format migration".into());
    }

    Ok(migrating_catalog)
}

#[cfg(test)]
mod tests;
