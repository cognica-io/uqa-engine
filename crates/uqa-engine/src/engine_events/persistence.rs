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

use crate::engine_capabilities::RelationLookupMode;
use crate::engine_open::CatalogRestoreMode;
use crate::{
    CatalogFacade, Engine, RelationIdentity, StorageBackendError, StorageBackendResult,
    RULES_METADATA_KEY, TRIGGERS_METADATA_KEY,
};

use super::{
    StoredRule, StoredRuleCatalog, StoredTrigger, StoredTriggerCatalog, RULE_CATALOG_FORMAT_VERSION,
};

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

    fn trigger_relation_persistence(
        &self,
        relation: &RelationIdentity,
    ) -> Option<RelationPersistence> {
        self.storage
            .tables
            .read()
            .get(relation)
            .map(|table| table.persistence)
            .or_else(|| {
                self.durable
                    .views
                    .read()
                    .get(relation)
                    .map(|view| view.persistence)
            })
            .or_else(|| {
                self.durable
                    .foreign_tables
                    .read()
                    .contains_key(relation)
                    .then_some(RelationPersistence::Permanent)
            })
    }

    pub(super) fn persist_rule_catalog_snapshot(
        &self,
        rules: &BTreeMap<RelationIdentity, BTreeMap<String, StoredRule>>,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let snapshot = StoredRuleCatalog {
            format_version: RULE_CATALOG_FORMAT_VERSION,
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
        mode: CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        let stored = match catalog.get_metadata(RULES_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<StoredRuleCatalog>(&json)?,
            None => StoredRuleCatalog::default(),
        };
        if stored.format_version > RULE_CATALOG_FORMAT_VERSION {
            return Err(StorageBackendError::Other(format!(
                "rule catalog format {} is newer than supported format {RULE_CATALOG_FORMAT_VERSION}",
                stored.format_version
            )));
        }
        let migrating_catalog = stored.format_version < RULE_CATALOG_FORMAT_VERSION;
        if migrating_catalog && !mode.allows_migration() {
            return Err(StorageBackendError::Other(
                "rule catalog requires an initial-open format migration".into(),
            ));
        }
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
            let persisted_definition = if migrating_catalog {
                None
            } else {
                Some(serde_json::to_string(&rule.definition)?)
            };
            if let Some(condition) = &mut rule.definition.condition {
                condition.upgrade_legacy_serialized_dispatches();
            }
            for action in &mut rule.definition.actions {
                action.upgrade_legacy_serialized_dispatches();
            }
            let stored_condition_plan = rule.condition_plan.clone();
            let stored_condition_binding = rule.condition_binding.clone();
            let (relation, condition_plan, condition_binding, dependencies) = self
                .validate_rule_definition(
                    &mut rule.definition,
                    RelationLookupMode::Bound,
                    stored_condition_plan.as_ref(),
                    stored_condition_binding.as_ref(),
                )
                .map_err(|error| {
                    StorageBackendError::Other(format!("restore rule catalog: {error}"))
                })?;
            if !migrating_catalog && rule.dependencies.as_ref() != Some(&dependencies) {
                return Err(StorageBackendError::Other(format!(
                    "restore rule catalog: persisted dependencies for rule `{}` do not match its definition",
                    rule.definition.name
                )));
            }
            if let Some(persisted_definition) = persisted_definition {
                let validated_definition = serde_json::to_string(&rule.definition)?;
                if persisted_definition != validated_definition {
                    return Err(StorageBackendError::Other(format!(
                        "restore rule catalog: persisted definition for rule `{}` is not fully bound",
                        rule.definition.name
                    )));
                }
            }
            rule.condition_plan = condition_plan;
            rule.condition_binding = condition_binding;
            rule.dependencies = Some(dependencies);
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
        if migrating_catalog {
            let rules = self.durable.rules.read();
            self.persist_rule_catalog_snapshot(&rules)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        Ok(())
    }

    pub(super) fn persist_trigger_catalog_snapshot(
        &self,
        triggers: &BTreeMap<RelationIdentity, BTreeMap<String, StoredTrigger>>,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let snapshot = StoredTriggerCatalog {
            triggers: triggers
                .iter()
                .filter(|(relation, _)| {
                    self.trigger_relation_persistence(relation)
                        .is_some_and(|persistence| persistence != RelationPersistence::Temporary)
                })
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
        mode: CatalogRestoreMode,
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
                self.trigger_relation_persistence(relation) == Some(RelationPersistence::Temporary)
            })
            .map(|(relation, entries)| (relation.clone(), entries.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut triggers = temporary_triggers;
        let mut migrated = false;
        for mut trigger in stored.triggers {
            if trigger.definition.constraint && trigger.constraint_name.is_none() {
                trigger.constraint_name = Some(trigger.definition.name.clone());
            }
            if let Some(condition) = &mut trigger.definition.when {
                condition.upgrade_legacy_serialized_dispatches();
            }
            let (relation, condition_routine_bindings_changed) = self
                .validate_trigger_definition(&mut trigger.definition, RelationLookupMode::Bound)
                .map_err(|error| {
                    StorageBackendError::Other(format!("restore trigger catalog: {error}"))
                })?;
            if condition_routine_bindings_changed {
                if !mode.allows_migration() {
                    return Err(StorageBackendError::Other(format!(
                        "trigger `{}` WHEN condition requires an initial-open routine-identity migration",
                        trigger.definition.name
                    )));
                }
                migrated = true;
            }
            let function_object_id = self
                .resolve_trigger_function(&trigger.definition.function, RelationLookupMode::Bound)
                .map_err(|error| {
                    StorageBackendError::Other(format!(
                        "restore trigger function identity: {error}"
                    ))
                })?
                .def
                .object_id
                .ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "restore trigger catalog: function `{}` has no object identity",
                        trigger.definition.function
                    ))
                })?;
            if trigger
                .function_object_id
                .is_some_and(|stored| stored != function_object_id)
            {
                return Err(StorageBackendError::Other(format!(
                    "restore trigger catalog: function identity for trigger `{}` does not match `{}`",
                    trigger.definition.name, trigger.definition.function
                )));
            }
            if trigger.function_object_id.is_none() {
                if !mode.allows_migration() {
                    return Err(StorageBackendError::Other(format!(
                        "trigger `{}` requires an initial-open function-identity migration",
                        trigger.definition.name
                    )));
                }
                trigger.function_object_id = Some(function_object_id);
                migrated = true;
            }
            if trigger.object_id.is_none() {
                if !mode.allows_migration() {
                    return Err(StorageBackendError::Other(format!(
                        "trigger `{}` requires an initial-open object-identity migration",
                        trigger.definition.name
                    )));
                }
                trigger.object_id = Some(legacy_trigger_object_id(&trigger.definition));
                migrated = true;
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
        if migrated {
            let triggers = self.durable.triggers.read();
            self.persist_trigger_catalog_snapshot(&triggers)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
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
