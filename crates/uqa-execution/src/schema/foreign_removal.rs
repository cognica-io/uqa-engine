//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign table removal, owned-sequence preflight, and durable publication inside the caller's transaction.
use crate::catalog::{
    foreign::lookup::ForeignLookupContext,
    sequence_introspection::{ownership, SequenceIntrospectionCatalog},
};
use crate::schema::{
    events::context::EventLifecycleContext, foreign_table_alteration::ForeignTableAlterPublication,
    publication::dependencies::CatalogPublicationChanges, removal::RelationRemovalSequences,
};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};
pub trait ForeignSequenceDependents {
    fn sequence_external_dependents_for_owner_drop(
        &self,
        sequence: &str,
        targets: &BTreeSet<String>,
    ) -> StorageBackendResult<Vec<String>>;
}
pub struct ForeignTableRemovalContext<'a> {
    pub lookup: ForeignLookupContext<'a>,
    pub publication: &'a dyn ForeignTableAlterPublication,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub events: EventLifecycleContext<'a>,
    pub owners: &'a dyn SequenceIntrospectionCatalog,
    pub dependencies: &'a dyn ForeignSequenceDependents,
    pub sequences: &'a dyn RelationRemovalSequences,
}
impl ForeignTableRemovalContext<'_> {
    pub fn drop_foreign_table(&self, name: &str) -> Result<bool, String> {
        self.lookup
            .state
            .synchronize_catalog_registries()
            .map_err(|error| format!("refresh FDW catalog: {error}"))?;
        let Some(canonical) = self
            .lookup
            .resolve_foreign_table_name(name)
            .map_err(|error| format!("resolve foreign table: {error}"))?
        else {
            return Ok(false);
        };
        let tables = vec![canonical.clone()];
        let targets = std::collections::BTreeSet::from([canonical.clone()]);
        let owned_sequences = self
            .foreign_table_owned_sequence_names(&tables)
            .map_err(|error| format!("resolve owned sequences: {error}"))?;
        for sequence in &owned_sequences {
            let dependents = self
                .dependencies
                .sequence_external_dependents_for_owner_drop(sequence, &targets)
                .map_err(|error| format!("inspect owned sequence `{sequence}`: {error}"))?;
            if !dependents.is_empty() {
                return Err(format!(
                        "foreign table `{canonical}` has owned sequence `{sequence}` with dependent object(s) `{}`",
                        dependents.join("`, `")
                    ));
            }
        }
        if !self.drop_foreign_table_inner(&canonical)? {
            return Err(format!(
                "foreign table `{canonical}` disappeared after DROP preflight"
            ));
        }
        for sequence in owned_sequences {
            self.sequences
                .drop_owned_sequence(&sequence, false)
                .map_err(|error| format!("drop owned sequence `{sequence}`: {error}"))?;
        }
        Ok(true)
    }
    pub fn drop_foreign_table_inner(&self, name: &str) -> Result<bool, String> {
        self.lookup
            .state
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let Some(name) = self
            .lookup
            .resolve_foreign_table_name(name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
        else {
            return Ok(false);
        };
        let relation = RelationIdentity::from_legacy_name(&name)?;
        if !self.lookup.registry.tables().contains_key(&relation) {
            return Err(format!("Foreign table `{name}` disappeared before drop"));
        }
        if !self.lookup.registry.security().contains_key(&relation) {
            return Err(format!(
                "Foreign table `{name}` has no loaded security metadata"
            ));
        }
        self.events
            .drop_relation_events_inner(&relation)
            .map_err(|error| format!("drop foreign table `{name}` events: {error}"))?;
        if let Some(catalog) = self.catalog {
            catalog
                .drop_foreign_table(&relation)
                .map_err(|err| format!("drop foreign table `{name}`: {err}"))?;
        }
        self.publication.memory_tables_write().remove(&relation);
        let mut tables = self.publication.tables_write();
        let mut table_security = self.publication.security_write();
        let removed = tables.remove(&relation).is_some();
        table_security.remove(&relation);
        drop(table_security);
        drop(tables);
        if removed {
            self.changes.catalog_registry_changed();
        }
        Ok(removed)
    }
    pub fn foreign_table_owned_sequence_names(
        &self,
        table_names: &[String],
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        let mut table_object_ids = std::collections::BTreeSet::new();
        let tables = self.lookup.registry.tables();
        for table_name in table_names {
            let relation = RelationIdentity::from_legacy_name(table_name)
                .map_err(StorageBackendError::Other)?;
            let table = tables.get(&relation).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "foreign table `{table_name}` disappeared while resolving owned sequences"
                ))
            })?;
            table_object_ids.insert(table.object_id);
        }
        drop(tables);
        ownership::sequence_names_owned_by_tables(self.owners, &table_object_ids)
    }
}
