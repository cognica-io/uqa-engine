//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable trigger metadata serialization and restoration.

use std::collections::BTreeMap;

use uqa_sql::ast::RelationPersistence;
use uqa_sql::SQLError;

use crate::{
    CatalogFacade, Engine, RelationIdentity, StorageBackendError, StorageBackendResult,
    TRIGGERS_METADATA_KEY,
};

use super::{StoredTrigger, StoredTriggerCatalog};

impl Engine {
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
        let Some(json) = catalog.get_metadata(TRIGGERS_METADATA_KEY)? else {
            return Ok(());
        };
        let stored = serde_json::from_str::<StoredTriggerCatalog>(&json)?;
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
            let relation = self
                .validate_trigger_definition(&mut trigger.definition)
                .map_err(|error| {
                    StorageBackendError::Other(format!("restore trigger catalog: {error}"))
                })?;
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
