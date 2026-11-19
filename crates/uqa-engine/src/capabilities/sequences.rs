//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind sequence catalog consumers to session namespaces and durable registry state.
use crate::{state::BoundSequenceSecurity, Engine, SequenceState};
use uqa_core::RelationIdentity;
use uqa_execution::schema::sequences::{
    creation::{SequenceCreationContext, SequenceCreationNamespace, SequenceCreationPublication},
    implicit::{ImplicitSequenceContext, ImplicitSequencePublication},
};
use uqa_sql::schema::sequences::ownership::{SequenceOwnerCatalog, SequenceOwnerColumns};
use uqa_sql::{ast::RelationPersistence, SQLError};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn sequence_creation_context(&self) -> SequenceCreationContext<'_> {
        SequenceCreationContext {
            identities: self.catalog_identity_reservation_context(),
            creation: self.relation_creation_context(),
            namespace: self,
            owners: self,
            publication: self,
        }
    }
    pub(crate) fn implicit_sequence_context(&self) -> ImplicitSequenceContext<'_> {
        ImplicitSequenceContext {
            namespace: self,
            owners: self,
            publication: self,
        }
    }
}

impl SequenceCreationNamespace for Engine {
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
        state: SequenceState,
        persistence: RelationPersistence,
    ) -> Result<(), SQLError> {
        self.create_implicit_sequence_with_persistence(name, state, persistence)
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
            .foreign_catalog
            .tables()
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
        role_owner: uqa_core::catalog_role::RoleIdentity,
        catalog_oid: u32,
    ) -> Result<bool, SQLError> {
        let security = BoundSequenceSecurity::owner(role_owner);
        let object_id = crate::new_sequence_object_id().map_err(|error| {
            SQLError::Internal(format!("allocate sequence `{name}` identity: {error}"))
        })?;
        state.definition_generation = object_id;
        let created = uqa_execution::catalog::sequence::registry::publication::create(
            &mut self.durable.sequences.write(),
            self.storage.catalog.as_deref(),
            name,
            relation,
            uqa_execution::catalog::sequence::registry::SequenceRecord {
                state,
                object_id,
                persistence,
                catalog_oid: Some(catalog_oid),
            },
            security,
        )?;
        if !created {
            return Ok(false);
        }
        self.note_catalog_registry_changed();
        self.note_prepared_catalog_change(
            uqa_execution::statement::prepared::invalidation::PreparedCatalogChange::Relation(
                catalog_oid,
            ),
        );
        Ok(true)
    }
}

impl Engine {
    pub(crate) fn implicit_ownership_context(
        &self,
    ) -> uqa_execution::schema::sequences::ownership::ImplicitOwnershipContext<'_> {
        uqa_execution::schema::sequences::ownership::ImplicitOwnershipContext {
            owners: self,
            names: self,
            tables: self,
            publication: self,
        }
    }
}
impl uqa_sql::schema::sequences::implicit_ownership::StoredSequenceNames for Engine {
    fn stored_sequence_name_with_control(
        &self,
        reference: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        uqa_sql::schema::sequences::names::resolve_stored_sequence_reference_with_control(
            self, reference, control,
        )
    }
    fn stored_sequence_name(&self, reference: &str) -> Result<String, String> {
        self.resolve_stored_sequence_reference_from_loaded_registry(reference)
            .map_err(|error| error.to_string())
    }
}
impl uqa_execution::schema::sequences::ownership::ImplicitOwnerTables for Engine {
    fn table_owner_columns(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<([u8; 16], Vec<uqa_sql::ast::ColumnDef>)>> {
        self.try_table(table)
            .map(|table| table.map(|table| (table.object_id(), table.columns.read().clone())))
    }
}
impl uqa_execution::schema::sequences::ownership::ImplicitOwnerPublication for Engine {
    fn attach_owner(
        &self,
        sequence: &str,
        owner: uqa_storage::SequenceOwner,
    ) -> Result<(), SQLError> {
        self.attach_sequence_owner_identity(sequence, owner)
    }
}

impl uqa_sql::schema::sequences::lifecycle::SequenceLifecycleCatalog for Engine {
    fn temporary_schema_name(&self) -> String {
        Engine::temporary_schema_name(self)
    }
    fn sequence_is_owned(&self, relation: &RelationIdentity) -> bool {
        self.durable
            .sequences
            .read()
            .state(relation)
            .is_some_and(|state| state.owner.is_some())
    }
    fn relation_kind_at(&self, name: &str) -> Result<Option<&'static str>, String> {
        Engine::relation_kind_at(self, name).map_err(|error| error.to_string())
    }
}
impl uqa_sql::schema::sequences::names::StoredSequenceRegistry for Engine {
    fn visit_sequence_names_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
        visit: &mut dyn FnMut(&RelationIdentity) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        for name in self.durable.sequences.read().keys() {
            control.check()?;
            visit(name)?;
        }
        Ok(())
    }
    fn contains_sequence(&self, relation: &RelationIdentity) -> bool {
        self.durable.sequences.read().contains_key(relation)
    }
    fn sequence_names(&self) -> Vec<RelationIdentity> {
        self.durable.sequences.read().keys().cloned().collect()
    }
}

impl Engine {
    pub(crate) fn sequence_definition_context(
        &self,
    ) -> uqa_execution::schema::sequences::alteration::SequenceDefinitionContext<'_> {
        uqa_execution::schema::sequences::alteration::SequenceDefinitionContext {
            catalog: self,
            owners: self,
            publication: self,
            markers: self.schema_dependency_publication_context(),
            new_generation: crate::new_sequence_definition_generation,
        }
    }
}
impl uqa_execution::schema::sequences::alteration::SequenceDefinitionCatalog for Engine {
    fn object_id(&self, relation: &RelationIdentity) -> Option<[u8; 16]> {
        self.durable.sequences.read().object_id(relation).copied()
    }
    fn state(&self, relation: &RelationIdentity) -> Result<Option<SequenceState>, SQLError> {
        let Some(state) = self.durable.sequences.read().state(relation).copied() else {
            return Ok(None);
        };
        let object_id = self.durable.sequences.read().object_id(relation).copied();
        let (Some(positions), Some(object_id)) = (self.shared_sequence_positions(), object_id)
        else {
            return Ok(Some(state));
        };
        Ok(Some(state.at_position(
            positions.sequence_position(state.position_key(object_id))?,
        )))
    }
    fn owner_target(&self, owner: uqa_storage::SequenceOwner) -> Option<(String, String, bool)> {
        self.sequence_owner_target(owner)
    }
}
impl uqa_execution::schema::sequences::alteration::SequenceDefinitionPublication for Engine {
    fn replace_sequence(
        &self,
        name: &str,
        relation: &RelationIdentity,
        object_id: [u8; 16],
        persistence: RelationPersistence,
        state: SequenceState,
        invalidate_current_cache: bool,
    ) -> Result<(), SQLError> {
        self.persist_sequence_state_replacement(
            name,
            relation,
            object_id,
            persistence,
            state,
            invalidate_current_cache,
        )
    }
}

impl Engine {
    pub(crate) fn sequence_role_ownership_context(
        &self,
    ) -> uqa_execution::schema::sequences::role_ownership::SequenceRoleOwnershipContext<'_> {
        uqa_execution::schema::sequences::role_ownership::SequenceRoleOwnershipContext {
            locks: self,
            writer: self,
            roles: self,
            session: self,
            schemas: self,
            metadata: self,
            security: self,
            changes: self,
        }
    }
}
impl uqa_execution::schema::sequences::role_ownership::SequenceSecurityPublication for Engine {
    fn security(&self, relation: &RelationIdentity) -> Option<BoundSequenceSecurity> {
        self.durable.sequences.security().get(relation).cloned()
    }
    fn persist_security(
        &self,
        name: &str,
        relation: &RelationIdentity,
        security: &BoundSequenceSecurity,
    ) -> Result<(), SQLError> {
        self.persist_sequence_security(name, relation, security)
    }
    fn sequence_catalog(&self) -> uqa_execution::catalog::sequence::registry::SequenceCatalog {
        self.durable.sequences.read().clone()
    }
    fn publish_sequences(
        &self,
        catalog: uqa_execution::catalog::sequence::registry::SequenceCatalog,
    ) {
        *self.durable.sequences.write() = catalog;
    }
}

impl Engine {
    pub(crate) fn sequence_lifecycle_context(
        &self,
    ) -> uqa_execution::schema::sequences::lifecycle::SequenceLifecycleContext<'_> {
        uqa_execution::schema::sequences::lifecycle::SequenceLifecycleContext {
            analysis: self,
            creation: self.relation_creation_context(),
            schemas: self.schema_dependency_publication_context(),
            views: self.view_sequence_rewrite_context(),
            state: self,
            events: self.event_catalog_context(),
            changes: self,
        }
    }
    pub(crate) fn sequence_alter_context(
        &self,
    ) -> uqa_execution::schema::sequences::dispatch::SequenceAlterContext<'_> {
        uqa_execution::schema::sequences::dispatch::SequenceAlterContext {
            catalog: self,
            authority: self.table_privilege_context(),
            definition: self.sequence_definition_context(),
            roles: self.sequence_role_ownership_context(),
            lifecycle: self.sequence_lifecycle_context(),
        }
    }
}
impl uqa_execution::schema::sequences::entry::SequenceCreationTransactions for Engine {
    fn with_sequence_creation(
        &self,
        write: uqa_execution::schema::sequences::entry::SequenceCreationWrite<'_>,
    ) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.sequence_creation_context()))
    }
}
impl uqa_execution::schema::sequences::entry::SequenceAlterTransactions for Engine {
    fn with_sequence_write(
        &self,
        write: uqa_execution::schema::sequences::entry::SequenceAlterWrite<'_>,
    ) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.sequence_alter_context()))
    }
}
impl uqa_execution::schema::sequences::lifecycle::SequenceStateRename for Engine {
    fn move_state(
        &self,
        source: &RelationIdentity,
        target: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.move_sequence_state(source, target)?;
        self.note_prepared_relation_change(target);
        Ok(())
    }
}
impl uqa_execution::schema::sequences::dispatch::SequenceCommandCatalog for Engine {
    fn resolve_visible_relation(
        &self,
        name: &str,
    ) -> Result<uqa_sql::catalog::resolution::RelationResolution, SQLError> {
        self.resolve_visible_relation_kind(name)
    }
    fn sequence_persistence(&self, relation: &RelationIdentity) -> RelationPersistence {
        self.durable
            .sequences
            .read()
            .persistence(relation)
            .unwrap_or_default()
    }
}
