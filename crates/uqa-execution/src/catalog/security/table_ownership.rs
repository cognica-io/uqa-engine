//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer table ownership after authorization, saving sequences before table publication.
use super::{
    roles::dependencies::{prepare_role_owner, RoleDependencyCandidate},
    table_grants::context::TableSecurityWrite,
    table_inquiry::TablePrivilegeState,
};
use crate::schema::{
    namespaces::SchemaStatementWriter,
    publication::dependencies::CatalogPublicationChanges,
    relation_alteration::RoleTransferContext,
    sequences::role_ownership::{
        table_owned_sequence_owner_updates, OwnedSequenceSecurityCatalog,
        OwnedSequenceSecurityWrite, SequenceSecurityPublication,
    },
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, RelationPersistence, TableConstraintSet},
    catalog::security::{ownership::OwnerChangeAuthority, table::rewrite_acl_owner, TableSecurity},
    SQLError,
};
use uqa_storage::StorageBackendResult;
pub struct TableOwnerSchema {
    pub columns: Vec<ColumnDef>,
    pub constraints: TableConstraintSet,
}
pub trait TableOwnerState: TablePrivilegeState {
    fn object_id(&self) -> [u8; 16];
    fn persistence(&self) -> RelationPersistence;
    fn schema(&self) -> TableOwnerSchema;
    fn persist_schema(
        &self,
        name: &str,
        schema: &TableOwnerSchema,
        security: &TableSecurity,
    ) -> StorageBackendResult<()>;
    fn security_write(&self) -> TableSecurityWrite<'_>;
}
pub trait TableOwnerRegistry {
    fn table(&self, relation: &RelationIdentity) -> Option<Box<dyn TableOwnerState + '_>>;
}
type BoundTableOwner<'a> = (RelationIdentity, Box<dyn TableOwnerState + 'a>);
pub trait TableOwnerPublication {
    fn sequence_security_write(&self) -> OwnedSequenceSecurityWrite<'_>;
}
pub struct TableOwnershipContext<'a> {
    pub writer: &'a dyn SchemaStatementWriter,
    pub tables: &'a dyn TableOwnerRegistry,
    pub roles: RoleTransferContext<'a>,
    pub owned_sequences: &'a dyn OwnedSequenceSecurityCatalog,
    pub sequences: &'a dyn SequenceSecurityPublication,
    pub publication: &'a dyn TableOwnerPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
}
impl TableOwnershipContext<'_> {
    pub fn alter_table_role_owner(
        &self,
        name: &str,
        requested_owner: &str,
    ) -> Result<(), SQLError> {
        // The ALTER entry retains the table lock; bind the new owner from the catalog current after that wait.
        self.roles.locks.refresh_shared_catalog()?;
        let owner = self.roles.bind(requested_owner)?;
        let current_user = self.roles.session.current_user_name();
        let RoleDependencyCandidate {
            roles,
            memberships,
            value,
            ..
        } = prepare_role_owner(
            self.roles.lock_context(),
            &owner,
            || self.writer.prepare_writer(),
            |roles, memberships| {
                let (relation, table) = self.bound_table_for_security(name)?;
                let mut security = table.security();
                if security.role_owner == owner.name {
                    return Ok(None);
                }
                let authority = OwnerChangeAuthority {
                    roles,
                    memberships,
                    current_user: &current_user,
                    new_owner: &owner.name,
                };
                authority.require_owner_change(&security.role_owner, "table", &relation.name)?;
                authority.require_schema_create(self.roles.schemas, &relation.schema)?;
                rewrite_acl_owner(&mut security, &owner.name);
                Ok(Some((table, security)))
            },
        )?;
        let Some((table, table_security)) = value else {
            return Ok(());
        };

        let sequence_updates = table_owned_sequence_owner_updates(
            self.owned_sequences,
            table.object_id(),
            &owner.name,
        )?;
        for (sequence, security) in &sequence_updates {
            self.sequences
                .persist_security(&sequence.qualified_name(), sequence, security)?;
        }
        let schema = table.schema();
        table
            .persist_schema(name, &schema, &table_security)
            .map_err(|error| SQLError::Internal(format!("persist table owner: {error}")))?;

        table.security_write().clone_from(&table_security);
        let sequence_changed = !sequence_updates.is_empty();
        if sequence_changed {
            let mut registry = self.publication.sequence_security_write();
            for (sequence, security) in sequence_updates {
                registry.insert(sequence, security);
            }
            drop(registry);
        }
        let temporary = table.persistence() == RelationPersistence::Temporary;
        drop(table);
        drop(memberships);
        drop(roles);
        if sequence_changed {
            self.changes.catalog_registry_changed();
        }
        if temporary {
            self.changes.table_catalog_changed();
        }
        Ok(())
    }
    fn bound_table_for_security(&self, name: &str) -> Result<BoundTableOwner<'_>, SQLError> {
        let relation = RelationIdentity::from_legacy_name(name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?;
        let table = self
            .tables
            .table(&relation)
            .ok_or_else(|| SQLError::Internal(format!("table `{name}` disappeared")))?;
        Ok((relation, table))
    }
}
