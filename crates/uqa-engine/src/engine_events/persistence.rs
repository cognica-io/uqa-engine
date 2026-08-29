//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable trigger metadata serialization and restoration.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use uqa_sql::ast::RelationPersistence;
use uqa_sql::SQLError;

use crate::{
    CatalogFacade, Engine, RelationIdentity, StorageBackendError, StorageBackendResult,
    RULES_METADATA_KEY, TRIGGERS_METADATA_KEY,
};

use super::{StoredRule, StoredRuleCatalog, StoredTrigger, StoredTriggerCatalog};

impl Engine {
    fn rule_relation_is_temporary(&self, relation: &RelationIdentity) -> bool {
        self.storage
            .tables
            .read()
            .get(relation)
            .is_some_and(|table| table.persistence == RelationPersistence::Temporary)
            || self
                .durable
                .views
                .read()
                .get(relation)
                .is_some_and(|view| view.persistence == RelationPersistence::Temporary)
    }

    pub(super) fn persist_rule_catalog_snapshot(
        &self,
        rules: &BTreeMap<RelationIdentity, BTreeMap<String, StoredRule>>,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let snapshot = StoredRuleCatalog {
            rules: rules
                .iter()
                .filter(|(relation, _)| !self.rule_relation_is_temporary(relation))
                .flat_map(|(_, entries)| entries.values().cloned())
                .collect(),
        };
        let json = serde_json::to_string(&snapshot)
            .map_err(|error| SQLError::Internal(format!("serialize rule catalog: {error}")))?;
        catalog
            .set_metadata(RULES_METADATA_KEY, &json)
            .map_err(|error| SQLError::Internal(format!("persist rule catalog: {error}")))
    }

    pub(crate) fn restore_rules_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let stored = match catalog.get_metadata(RULES_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<StoredRuleCatalog>(&json)?,
            None => StoredRuleCatalog::default(),
        };
        let temporary_rules = self
            .durable
            .rules
            .read()
            .iter()
            .filter(|(relation, _)| self.rule_relation_is_temporary(relation))
            .map(|(relation, entries)| (relation.clone(), entries.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut rules = temporary_rules;
        for mut rule in stored.rules {
            if let Some(condition) = &mut rule.definition.condition {
                condition.upgrade_legacy_serialized_dispatches();
            }
            for action in &mut rule.definition.actions {
                action.upgrade_legacy_serialized_dispatches();
            }
            let relation = self
                .validate_rule_definition(&mut rule.definition)
                .map_err(|error| {
                    StorageBackendError::Other(format!("restore rule catalog: {error}"))
                })?;
            let name = rule.definition.name.clone();
            if rules
                .entry(relation)
                .or_default()
                .insert(name.clone(), rule)
                .is_some()
            {
                return Err(StorageBackendError::Other(format!(
                    "duplicate persisted rule `{name}`"
                )));
            }
        }
        *self.durable.rules.write() = rules;
        Ok(())
    }

    pub(super) fn persist_trigger_catalog_snapshot(
        &self,
        triggers: &BTreeMap<RelationIdentity, BTreeMap<String, StoredTrigger>>,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let is_persistent = |relation: &RelationIdentity| {
            self.storage
                .tables
                .read()
                .get(relation)
                .is_some_and(|table| table.persistence != RelationPersistence::Temporary)
        };
        let snapshot = StoredTriggerCatalog {
            triggers: triggers
                .iter()
                .filter(|(relation, _)| is_persistent(relation))
                .flat_map(|(_, entries)| entries.values().cloned())
                .collect(),
        };
        let json = serde_json::to_string(&snapshot)
            .map_err(|error| SQLError::Internal(format!("serialize trigger catalog: {error}")))?;
        catalog
            .set_metadata(TRIGGERS_METADATA_KEY, &json)
            .map_err(|error| SQLError::Internal(format!("persist trigger catalog: {error}")))
    }

    pub(crate) fn restore_triggers_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let stored = match catalog.get_metadata(TRIGGERS_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<StoredTriggerCatalog>(&json)?,
            None => StoredTriggerCatalog::default(),
        };
        let temporary_triggers = self
            .durable
            .triggers
            .read()
            .iter()
            .filter(|(relation, _)| {
                self.storage
                    .tables
                    .read()
                    .get(*relation)
                    .is_some_and(|table| table.persistence == RelationPersistence::Temporary)
            })
            .map(|(relation, entries)| (relation.clone(), entries.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut triggers = temporary_triggers;
        for mut trigger in stored.triggers {
            if trigger.definition.constraint && trigger.constraint_name.is_none() {
                trigger.constraint_name = Some(trigger.definition.name.clone());
            }
            if let Some(condition) = &mut trigger.definition.when {
                condition.upgrade_legacy_serialized_dispatches();
            }
            let relation = self
                .validate_trigger_definition(&mut trigger.definition)
                .map_err(|error| {
                    StorageBackendError::Other(format!("restore trigger catalog: {error}"))
                })?;
            if trigger.object_id.is_none() {
                trigger.object_id = Some(legacy_trigger_object_id(&trigger.definition));
            }
            let name = trigger.definition.name.clone();
            if triggers
                .entry(relation)
                .or_default()
                .insert(name.clone(), trigger)
                .is_some()
            {
                return Err(StorageBackendError::Other(format!(
                    "duplicate persisted trigger `{name}`"
                )));
            }
        }
        *self.durable.triggers.write() = triggers;
        Ok(())
    }
}

fn legacy_trigger_object_id(definition: &uqa_sql::ast::CreateTrigger) -> [u8; 16] {
    let mut digest = Sha256::new();
    digest.update(b"uqa:legacy-trigger-object-id\0");
    digest.update(definition.table.as_bytes());
    digest.update([0]);
    digest.update(definition.name.as_bytes());
    let digest = digest.finalize();
    let mut object_id = [0_u8; 16];
    object_id.copy_from_slice(&digest[..16]);
    object_id
}
