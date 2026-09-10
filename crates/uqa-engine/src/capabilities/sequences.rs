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

impl Engine {
    pub(crate) fn implicit_ownership_context(
        &self,
    ) -> uqa_execution::schema::sequences::ownership::ImplicitOwnershipContext<'_> {
        uqa_execution::schema::sequences::ownership::ImplicitOwnershipContext {
            names: self,
            tables: self,
            publication: self,
        }
    }
}
impl uqa_sql::schema::sequences::implicit_ownership::StoredSequenceNames for Engine {
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
            .get(relation)
            .is_some_and(|state| state.owner.is_some())
    }
    fn schema_exists(&self, schema: &str) -> bool {
        self.durable.schemas.read().contains_key(schema)
    }
    fn current_user_name(&self) -> String {
        Engine::current_user_name(self)
    }
    fn require_schema_create(&self, schema: &str, role: &str) -> Result<(), SQLError> {
        self.require_schema_privilege(
            schema,
            role,
            crate::schema_security::SchemaAclPrivilege::Create,
        )
    }
    fn relation_kind_at(&self, name: &str) -> Result<Option<&'static str>, String> {
        Engine::relation_kind_at(self, name).map_err(|error| error.to_string())
    }
}
impl uqa_sql::schema::sequences::names::StoredSequenceRegistry for Engine {
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
        self.durable
            .sequence_object_ids
            .read()
            .get(relation)
            .copied()
    }
    fn state(&self, relation: &RelationIdentity) -> Option<SequenceState> {
        self.durable.sequences.read().get(relation).copied()
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
            roles: self,
            session: self,
            access: self,
            schemas: self,
            metadata: self,
            security: self,
            changes: self,
        }
    }
}
impl uqa_execution::catalog::security::roles::RoleCatalogGuards for Engine {
    fn role_definitions(&self) -> uqa_execution::catalog::security::roles::RoleDefinitionRead<'_> {
        Box::new(self.durable.roles.read())
    }
    fn role_memberships(&self) -> uqa_execution::catalog::security::roles::RoleMembershipRead<'_> {
        Box::new(self.durable.role_memberships.read())
    }
}
impl uqa_sql::catalog::roles::RoleReferenceNames for Engine {
    fn current_user_name(&self) -> String {
        Engine::current_user_name(self)
    }
    fn session_user_name(&self) -> String {
        Engine::session_user_name(self)
    }
}
impl uqa_execution::schema::sequences::role_ownership::SequenceRoleAccess for Engine {
    fn ensure_sequence_owner(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<String, SQLError> {
        Engine::ensure_sequence_owner(self, name, relation)
    }
    fn current_user_is_superuser(&self) -> bool {
        Engine::current_user_is_superuser(self)
    }
}
impl uqa_execution::schema::sequences::role_ownership::SequenceSecurityPublication for Engine {
    fn security(&self, relation: &RelationIdentity) -> Option<SequenceSecurity> {
        self.durable.sequence_security.read().get(relation).cloned()
    }
    fn persist_security(
        &self,
        name: &str,
        relation: &RelationIdentity,
        security: &SequenceSecurity,
    ) -> Result<(), SQLError> {
        self.persist_sequence_security(name, relation, security)
    }
    fn publish_security(&self, relation: &RelationIdentity, security: SequenceSecurity) {
        self.durable
            .sequence_security
            .write()
            .insert(relation.clone(), security);
    }
}

impl Engine {
    pub(crate) fn sequence_lifecycle_context(
        &self,
    ) -> uqa_execution::schema::sequences::lifecycle::SequenceLifecycleContext<'_> {
        uqa_execution::schema::sequences::lifecycle::SequenceLifecycleContext {
            analysis: self,
            schemas: self.schema_dependency_publication_context(),
            views: self.view_sequence_rewrite_context(),
            state: self,
            catalog: self,
            refresh: self,
            events: self.event_catalog_context(),
            changes: self,
        }
    }
    pub(crate) fn sequence_alter_context(
        &self,
    ) -> uqa_execution::schema::sequences::dispatch::SequenceAlterContext<'_> {
        uqa_execution::schema::sequences::dispatch::SequenceAlterContext {
            catalog: self,
            definition: self.sequence_definition_context(),
            roles: self.sequence_role_ownership_context(),
            lifecycle: self.sequence_lifecycle_context(),
        }
    }
}
impl uqa_execution::schema::sequences::lifecycle::SequenceStateRename for Engine {
    fn move_state(
        &self,
        source: &RelationIdentity,
        target: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.move_sequence_state(source, target)
    }
}
impl uqa_execution::schema::sequences::lifecycle::SequenceRenameCatalog for Engine {
    fn has_catalog(&self) -> bool {
        self.storage.catalog.is_some()
    }
    fn rename_sequence_row(&self, source: &str, target: &str) -> StorageBackendResult<bool> {
        self.storage.catalog.as_ref().map_or(Ok(false), |catalog| {
            catalog.rename_sequence_row(source, target)
        })
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
            .sequence_persistence
            .read()
            .get(relation)
            .copied()
            .unwrap_or_default()
    }
}
