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
        let mut recorded_oids = false;
        let mut restored_expressions = false;
        for mut rule in stored.rules {
            let expression_migration =
                uqa_sql::catalog::events::restoration::restore_rule_expressions(
                    &mut rule,
                    self.analysis.routines,
                )
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            let constructor_migration =
                rule_constructor_migration(&mut rule.definition, self.analysis.routines)?;
            if (constructor_migration || expression_migration) && !allows_migration {
                return Err(StorageBackendError::Other(
                    "rule expressions require an initial-open migration".into(),
                ));
            }
            restored_expressions |= constructor_migration || expression_migration;
            let persisted_definition =
                if migrating_catalog || constructor_migration || expression_migration {
                    None
                } else {
                    Some(serde_json::to_string(&rule.definition)?)
                };
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
                restored_expressions |=
                    recorded_rule_bindings(&persisted_definition, &rule, allows_migration)?;
            }
            rule.condition_plan = condition_plan;
            rule.condition_binding = condition_binding;
            rule.dependencies = Some(dependencies);
            // A rule stored before OIDs were recorded keeps the OID its name derives, so renaming it later does not change it.
            if rule.catalog_oid.is_none() && allows_migration {
                rule.catalog_oid = Some(crate::catalog::projection::legacy_rule_catalog_oid(&rule));
                recorded_oids = true;
            }
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
        if migrating_catalog || recorded_oids || restored_expressions {
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
            migrated |=
                restore_trigger_expression(&mut trigger, self.analysis.routines, allows_migration)?;
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

fn restore_trigger_expression(
    trigger: &mut StoredTrigger,
    routines: &dyn uqa_sql::FunctionTypeResolver,
    allows_migration: bool,
) -> StorageBackendResult<bool> {
    let mut changed = trigger
        .definition
        .when
        .as_mut()
        .is_some_and(uqa_sql::ast::Expr::upgrade_legacy_serialized_dispatches);
    if let Some(condition) = &mut trigger.definition.when {
        changed |= uqa_sql::binding::legacy_patterns::restore_expression(condition, routines)
            .map_err(StorageBackendError::Other)?;
    }
    if changed && !allows_migration {
        return Err(StorageBackendError::Other(format!(
            "trigger `{}` WHEN condition requires an initial-open expression migration",
            trigger.definition.name
        )));
    }
    Ok(changed)
}

fn rule_constructor_migration(
    definition: &mut uqa_sql::ast::CreateRule,
    types: &dyn uqa_sql::FunctionTypeResolver,
) -> StorageBackendResult<bool> {
    use uqa_sql::type_resolution::composite_rows;
    let mut found = definition
        .condition
        .as_ref()
        .map(|condition| composite_rows::expression_requires_binding(condition, types))
        .transpose()
        .map_err(|error| StorageBackendError::Other(error.to_string()))?
        .unwrap_or(false);
    for action in &mut definition.actions {
        found |= composite_rows::statement_requires_binding(action, types)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    }
    Ok(found)
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
    let before: StoredTriggerCatalog = catalog
        .get_metadata(TRIGGERS_METADATA_KEY)
        .map_err(|error| {
            uqa_sql::catalog::errors::storage_error("read trigger predecessors", &error)
        })?
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .unwrap_or_default();
    let snapshot = model::stored_triggers_snapshot(triggers, relations);
    for trigger in before
        .triggers
        .iter()
        .chain(&snapshot.triggers)
        .filter(|trigger| trigger.definition.constraint)
    {
        crate::catalog::tuples::publication::queue_trigger(catalog, trigger).map_err(|error| {
            uqa_sql::catalog::errors::storage_error("publish constraint trigger", &error)
        })?;
    }
    let json = serde_json::to_string(&snapshot)
        .map_err(|error| SQLError::Internal(format!("serialize trigger catalog: {error}")))?;
    catalog
        .set_metadata(TRIGGERS_METADATA_KEY, &json)
        .map_err(|error| SQLError::Internal(format!("persist trigger catalog: {error}")))
}

/// Whether validating `rule`, whose definition was persisted as `persisted`, recorded the routine bindings of calls that a release resolving routines by name stored without one. Any other change means the persisted definition was not fully bound.
fn recorded_rule_bindings(
    persisted: &str,
    rule: &StoredRule,
    allows_migration: bool,
) -> StorageBackendResult<bool> {
    let validated = serde_json::to_string(&rule.definition)?;
    if persisted == validated {
        return Ok(false);
    }
    if !gained_only_routine_bindings(persisted, &validated)? {
        return Err(StorageBackendError::Other(format!(
            "restore rule catalog: persisted definition for rule `{}` is not fully bound",
            rule.definition.name
        )));
    }
    if !allows_migration {
        return Err(StorageBackendError::Other(format!(
            "rule `{}` requires an initial-open routine-identity migration",
            rule.definition.name
        )));
    }
    Ok(true)
}

/// Whether the serialized rule definition `validated` differs from the `persisted` one only by the routine bindings of calls stored without one.
fn gained_only_routine_bindings(persisted: &str, validated: &str) -> StorageBackendResult<bool> {
    let persisted: serde_json::Value = serde_json::from_str(persisted)?;
    let mut validated: serde_json::Value = serde_json::from_str(validated)?;
    drop_gained_routine_bindings(&persisted, &mut validated);
    Ok(persisted == validated)
}

/// Remove from `validated` every routine binding whose call `persisted` stores without one. Only a routine call carries an optional `binding`.
fn drop_gained_routine_bindings(persisted: &serde_json::Value, validated: &mut serde_json::Value) {
    match (persisted, validated) {
        (serde_json::Value::Object(persisted), serde_json::Value::Object(validated)) => {
            if !persisted.contains_key("binding") {
                validated.remove("binding");
            }
            for (key, value) in validated.iter_mut() {
                if let Some(persisted) = persisted.get(key) {
                    drop_gained_routine_bindings(persisted, value);
                }
            }
        }
        (serde_json::Value::Array(persisted), serde_json::Value::Array(validated)) => {
            for (persisted, validated) in persisted.iter().zip(validated.iter_mut()) {
                drop_gained_routine_bindings(persisted, validated);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
