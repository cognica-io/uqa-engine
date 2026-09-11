//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Event metadata I/O and catalog restoration inside the caller's existing transaction boundary.
use super::EventCatalogContext;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::RelationPersistence,
    catalog::{
        events::{
            definition::EventAnalysisContext,
            persistence::{
                self as model, EventRelationPersistence, StoredRuleCatalog, StoredTriggerCatalog,
            },
            reads::EventCatalogReads,
            StoredRule, StoredTrigger,
        },
        resolution::RelationLookupMode,
    },
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};
pub const TRIGGERS_METADATA_KEY: &str = "sql_triggers_json";
pub const RULES_METADATA_KEY: &str = "sql_rules_json";
pub struct EventRestoreContext<'a> {
    pub analysis: EventAnalysisContext<'a>,
    pub reads: &'a dyn EventCatalogReads,
    pub catalog: EventCatalogContext<'a>,
    pub relations: &'a dyn EventRelationPersistence,
}
impl EventRestoreContext<'_> {
    pub fn restore_rules_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
        allows_migration: bool,
    ) -> StorageBackendResult<()> {
        let stored = match catalog.get_metadata(RULES_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<StoredRuleCatalog>(&json)?,
            None => StoredRuleCatalog::default(),
        };
        let migrating_catalog =
            model::rule_catalog_requires_migration(stored.format_version, allows_migration)
                .map_err(StorageBackendError::Other)?;
        let temporary_rules = self
            .reads
            .read_rules()
            .iter()
            .filter(|(relation, _)| self.relations.rule_relation_is_temporary(relation))
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
                .analysis
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
        **self.catalog.registry.rules() = rules;
        if migrating_catalog {
            let rules = self.reads.read_rules();
            self.catalog
                .publication
                .persist_rules(&rules)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        Ok(())
    }

    pub fn restore_triggers_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
        allows_migration: bool,
    ) -> StorageBackendResult<()> {
        let stored = match catalog.get_metadata(TRIGGERS_METADATA_KEY)? {
            Some(json) => serde_json::from_str::<StoredTriggerCatalog>(&json)?,
            None => StoredTriggerCatalog::default(),
        };
        let temporary_triggers = self
            .reads
            .read_triggers()
            .iter()
            .filter(|(relation, _)| {
                self.relations.trigger_relation_persistence(relation)
                    == Some(RelationPersistence::Temporary)
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
                .analysis
                .validate_trigger_definition(&mut trigger.definition, RelationLookupMode::Bound)
                .map_err(|error| {
                    StorageBackendError::Other(format!("restore trigger catalog: {error}"))
                })?;
            if condition_routine_bindings_changed {
                if !allows_migration {
                    return Err(StorageBackendError::Other(format!(
                        "trigger `{}` WHEN condition requires an initial-open routine-identity migration",
                        trigger.definition.name
                    )));
                }
                migrated = true;
            }
            let function_object_id =
                uqa_sql::catalog::events::restoration::trigger_function_object_id(
                    &self.analysis,
                    &trigger.definition,
                )
                .map_err(StorageBackendError::Other)?;
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
                if !allows_migration {
                    return Err(StorageBackendError::Other(format!(
                        "trigger `{}` requires an initial-open function-identity migration",
                        trigger.definition.name
                    )));
                }
                trigger.function_object_id = Some(function_object_id);
                migrated = true;
            }
            if trigger.object_id.is_none() {
                if !allows_migration {
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
        **self.catalog.registry.triggers() = triggers;
        if migrated {
            let triggers = self.reads.read_triggers();
            self.catalog
                .publication
                .persist_triggers(&triggers)
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

pub fn persist_rule_catalog_snapshot(
    catalog: Option<&dyn CatalogFacade>,
    relations: &dyn EventRelationPersistence,
    rules: &BTreeMap<RelationIdentity, BTreeMap<String, StoredRule>>,
) -> Result<(), SQLError> {
    let Some(catalog) = catalog else {
        return Ok(());
    };
    let snapshot = model::stored_rules_snapshot(rules, relations);
    let json = serde_json::to_string(&snapshot)
        .map_err(|error| SQLError::Internal(format!("serialize rule catalog: {error}")))?;
    catalog
        .set_metadata(RULES_METADATA_KEY, &json)
        .map_err(|error| SQLError::Internal(format!("persist rule catalog: {error}")))
}

pub fn persist_trigger_catalog_snapshot(
    catalog: Option<&dyn CatalogFacade>,
    relations: &dyn EventRelationPersistence,
    triggers: &BTreeMap<RelationIdentity, BTreeMap<String, StoredTrigger>>,
) -> Result<(), SQLError> {
    let Some(catalog) = catalog else {
        return Ok(());
    };
    let snapshot = model::stored_triggers_snapshot(triggers, relations);
    let json = serde_json::to_string(&snapshot)
        .map_err(|error| SQLError::Internal(format!("serialize trigger catalog: {error}")))?;
    catalog
        .set_metadata(TRIGGERS_METADATA_KEY, &json)
        .map_err(|error| SQLError::Internal(format!("persist trigger catalog: {error}")))
}

#[cfg(test)]
mod tests;
