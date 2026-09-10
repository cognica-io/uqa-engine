//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind sequence catalog consumers to session namespaces and durable registry state.
use crate::{state::SequenceSecurity, Engine, SequenceState};
use uqa_core::RelationIdentity;
use uqa_execution::schema::sequences::{
    creation::{SequenceCreationContext, SequenceCreationNamespace, SequenceCreationPublication},
    implicit::{ImplicitSequenceContext, ImplicitSequencePublication},
};
use uqa_sql::schema::sequences::ownership::{SequenceOwnerCatalog, SequenceOwnerColumns};
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    SQLError,
};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn sequence_creation_context(&self) -> SequenceCreationContext<'_> {
        SequenceCreationContext {
            namespace: self,
            owners: self,
            publication: self,
        }
    }
    pub(crate) fn implicit_sequence_context(&self) -> ImplicitSequenceContext<'_> {
        ImplicitSequenceContext {
            namespace: self,
            publication: self,
        }
    }
}

impl SequenceCreationNamespace for Engine {
    fn temporary_name(&self, name: &str) -> Result<String, SQLError> {
        self.try_temporary_relation_name_for_create(name)
    }
    fn persistent_name(&self, name: &str) -> Result<String, SQLError> {
        self.try_relation_name_for_sql_create(name)
    }
    fn refresh_sequences(&self) -> StorageBackendResult<()> {
        self.refresh_sequences_from_catalog()
    }
    fn relation_exists(&self, name: &str) -> StorageBackendResult<bool> {
        self.relation_kind_at(name).map(|kind| kind.is_some())
    }
}
impl ImplicitSequencePublication for Engine {
    fn create_implicit_sequence(
        &self,
        name: &str,
        start: i64,
        increment: i64,
        data_type: SequenceDataType,
        persistence: RelationPersistence,
    ) -> Result<(), SQLError> {
        self.create_implicit_sequence_with_persistence(
            name,
            start,
            increment,
            data_type,
            persistence,
        )
    }
}
impl SequenceOwnerCatalog for Engine {
    fn resolve_owner_relation(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        self.try_resolve_visible_relation_kind(name)
    }
    fn owner_table_columns(
        &self,
        canonical: &str,
    ) -> Result<Option<SequenceOwnerColumns>, SQLError> {
        self.try_table(canonical)
            .map_err(|error| SQLError::Internal(format!("load table `{canonical}`: {error}")))
            .map(|table| table.map(|table| (table.object_id(), table.columns.read().clone())))
    }
    fn owner_foreign_columns(&self, relation: &RelationIdentity) -> Option<SequenceOwnerColumns> {
        self.durable
            .foreign_tables
            .read()
            .get(relation)
            .map(|table| (table.object_id, table.columns.clone()))
    }
}
impl SequenceCreationPublication for Engine {
    fn insert_sequence(
        &self,
        name: &str,
        relation: &RelationIdentity,
        mut state: SequenceState,
        persistence: RelationPersistence,
    ) -> Result<bool, SQLError> {
        let role_owner = self.current_user_name();
        let security = SequenceSecurity {
            role_owner,
            acl: None,
        };
        let object_id = crate::new_sequence_object_id().map_err(|error| {
            SQLError::Internal(format!("allocate sequence `{name}` identity: {error}"))
        })?;
        state.definition_generation = object_id;
        if persistence == RelationPersistence::Temporary {
            let seqs = self.durable.sequences.read();
            if seqs.contains_key(relation) {
                return Ok(false);
            }
        } else if let Some(catalog) = self.storage.catalog.as_ref() {
            let created = catalog
                .create_sequence_row(
                    &Self::sequence_row(name, object_id, state, persistence, &security).map_err(
                        |error| SQLError::Internal(format!("build sequence catalog row: {error}")),
                    )?,
                )
                .map_err(|error| {
                    SQLError::Internal(format!("persist sequence catalog: {error}"))
                })?;
            if !created {
                return Ok(false);
            }
        } else {
            let seqs = self.durable.sequences.read();
            if seqs.contains_key(relation) {
                return Ok(false);
            }
        }
        self.durable
            .sequences
            .write()
            .insert(relation.clone(), state);
        self.durable
            .sequence_object_ids
            .write()
            .insert(relation.clone(), object_id);
        self.durable
            .sequence_persistence
            .write()
            .insert(relation.clone(), persistence);
        self.durable
            .sequence_security
            .write()
            .insert(relation.clone(), security);
        self.note_catalog_registry_changed();
        Ok(true)
    }
}
