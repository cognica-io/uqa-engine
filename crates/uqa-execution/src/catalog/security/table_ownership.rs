//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer table ownership after authorization, saving sequences before table publication.
use super::{
    table_authorization::TableAuthorizationContext, table_grants::context::TableSecurityWrite,
    table_inquiry::TablePrivilegeState,
};
use crate::schema::{
    namespaces::SchemaStatementWriter,
    publication::dependencies::CatalogPublicationChanges,
    relation_alteration::{role_transfer_target, RoleTransferContext},
    sequences::role_ownership::{
        table_owned_sequence_owner_updates, OwnedSequenceSecurityCatalog,
        OwnedSequenceSecurityWrite, SequenceSecurityPublication,
    },
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, RelationPersistence, TableConstraintSet},
    catalog::security::{table::rewrite_acl_owner, TableSecurity},
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
    pub authorization: TableAuthorizationContext<'a>,
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
        self.writer.prepare_writer()?;
        let (relation, table) = self.bound_table_for_security(name)?;
        let current_owner = self.authorization.ensure_table_owner(name)?;
        let (new_owner, current_user_is_superuser) =
            role_transfer_target(&self.roles, requested_owner)?;
        if current_owner == new_owner {
            return Ok(());
        }
        if !current_user_is_superuser {
            self.roles
                .schemas
                .require_schema_create(&relation.schema, &new_owner)?;
        }

        let sequence_updates = table_owned_sequence_owner_updates(
            self.owned_sequences,
            table.object_id(),
            &new_owner,
        )?;
        for (sequence, security) in &sequence_updates {
            self.sequences
                .persist_security(&sequence.qualified_name(), sequence, security)?;
        }
        let schema = table.schema();
        let mut table_security = table.security();
        rewrite_acl_owner(&mut table_security, &new_owner);
        table
            .persist_schema(name, &schema, &table_security)
            .map_err(|error| SQLError::Internal(format!("persist table owner: {error}")))?;

        table.security_write().clone_from(&table_security);
        if !sequence_updates.is_empty() {
            let mut registry = self.publication.sequence_security_write();
            for (sequence, security) in sequence_updates {
                registry.insert(sequence, security);
            }
            drop(registry);
            self.changes.catalog_registry_changed();
        }
        if table.persistence() == uqa_sql::ast::RelationPersistence::Temporary {
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
