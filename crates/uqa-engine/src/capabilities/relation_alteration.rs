//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind relation metadata, rename dependencies and role-transfer catalogs to their current owners.
use crate::Engine;
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_execution::schema::{
    relation_alteration::{
        RelationAlterLocks, RelationRenameDependencies, RoleTargetSchemaAccess, RoleTransferContext,
    },
    sequences::role_ownership::{OwnedSequenceSecurityCatalog, OwnedSequenceSecurityRead},
};
use uqa_sql::{
    catalog::resolution::RelationResolution, schema::relation_alteration::RelationAlterNames,
    SQLError,
};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn role_transfer_context(&self) -> RoleTransferContext<'_> {
        RoleTransferContext {
            roles: self,
            session: self,
            schemas: self,
        }
    }
}
impl RelationAlterNames for Engine {
    fn resolve_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError> {
        self.resolve_visible_relation_kind(name)
    }
    fn relation_kind_at(&self, name: &str) -> Result<Option<&'static str>, String> {
        Engine::relation_kind_at(self, name).map_err(|error| error.to_string())
    }
}
impl RelationAlterLocks for Engine {
    fn lock_exclusive(&self, name: &str) -> Result<(), SQLError> {
        self.lock_relation(name, crate::row_locks::RelationLockMode::AccessExclusive)
    }
}
impl RelationRenameDependencies for Engine {
    fn rewrite_views(
        &self,
        renames: &BTreeMap<RelationIdentity, RelationIdentity>,
    ) -> StorageBackendResult<()> {
        self.rewrite_view_relation_references(renames)
    }
    fn rewrite_routines(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<(), String> {
        self.rewrite_routine_relation_references(from, to)
            .map_err(|error| error.to_string())
    }
    fn rename_events(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        self.rename_relation_events_inner(from, to)
    }
}
impl RoleTargetSchemaAccess for Engine {
    fn require_schema_create(&self, schema: &str, role: &str) -> Result<(), SQLError> {
        self.require_schema_privilege(
            schema,
            role,
            crate::schema_security::SchemaAclPrivilege::Create,
        )
    }
}
impl OwnedSequenceSecurityCatalog for Engine {
    fn owned_sequences(&self, table_object_id: [u8; 16]) -> Vec<RelationIdentity> {
        self.durable
            .sequences
            .read()
            .iter()
            .filter_map(|(relation, state)| {
                state
                    .owner
                    .is_some_and(|owner| owner.table_object_id == table_object_id)
                    .then_some(relation.clone())
            })
            .collect()
    }
    fn security_registry(&self) -> OwnedSequenceSecurityRead<'_> {
        Box::new(self.durable.sequence_security.read())
    }
}
