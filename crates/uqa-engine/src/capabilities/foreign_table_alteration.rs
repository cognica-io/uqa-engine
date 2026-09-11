//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind foreign-table alteration to current catalog values, provider writes and registry guards.
use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::{
    catalog::{foreign::StoredForeignTable, security::TableSecurity},
    schema::{
        foreign_table_alteration::{
            ForeignMemoryRegistryWrite, ForeignSecurityRegistryWrite, ForeignTableAlterAccess,
            ForeignTableAlterCatalog, ForeignTableAlterContext, ForeignTableAlterPublication,
            ForeignTableAlterTransactions, ForeignTableAlterWrite, ForeignTableOwnerWriter,
            ForeignTableRegistryWrite,
        },
        sequences::role_ownership::OwnedSequenceSecurityWrite,
    },
};
use uqa_sql::SQLError;
use uqa_storage::StorageBackendResult;

impl Engine {
    fn foreign_table_alter_context(&self) -> ForeignTableAlterContext<'_> {
        ForeignTableAlterContext {
            names: self,
            catalog: self,
            access: self,
            locks: self,
            writer: self,
            roles: self.role_transfer_context(),
            dependencies: self,
            owned_sequences: self,
            sequence_publication: self,
            publication: self,
            changes: self,
            notices: self.query_runtime_view().notices,
        }
    }
}
impl ForeignTableAlterTransactions for Engine {
    fn with_foreign_table_write(&self, write: ForeignTableAlterWrite<'_>) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.foreign_table_alter_context()))
    }
}
impl ForeignTableAlterCatalog for Engine {
    fn contains_table(&self, relation: &RelationIdentity) -> bool {
        self.durable.foreign_tables.read().contains_key(relation)
    }
    fn security(&self, relation: &RelationIdentity) -> Option<TableSecurity> {
        self.durable
            .foreign_table_security
            .read()
            .get(relation)
            .cloned()
    }
    fn table(&self, relation: &RelationIdentity) -> Option<StoredForeignTable> {
        self.durable.foreign_tables.read().get(relation).cloned()
    }
}
impl ForeignTableAlterAccess for Engine {
    fn ensure_owner(&self, name: &str) -> Result<String, SQLError> {
        self.ensure_foreign_table_owner(name)
    }
}
impl ForeignTableOwnerWriter for Engine {
    fn prepare_writer(&self) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer().map(|_| ())
    }
}
impl ForeignTableAlterPublication for Engine {
    fn persist_rename(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<Option<bool>> {
        self.storage
            .catalog
            .as_ref()
            .map(|catalog| catalog.rename_foreign_table(from, to))
            .transpose()
    }
    fn tables_write(&self) -> ForeignTableRegistryWrite<'_> {
        Box::new(self.durable.foreign_tables.write())
    }
    fn security_write(&self) -> ForeignSecurityRegistryWrite<'_> {
        Box::new(self.durable.foreign_table_security.write())
    }
    fn memory_tables_write(&self) -> ForeignMemoryRegistryWrite<'_> {
        Box::new(self.extensions.foreign_memory_tables.write())
    }
    fn sequence_security_write(&self) -> OwnedSequenceSecurityWrite<'_> {
        Box::new(self.durable.sequence_security.write())
    }
    fn persist_security(
        &self,
        relation: &RelationIdentity,
        security: &TableSecurity,
    ) -> Result<(), SQLError> {
        self.persist_foreign_table_security(relation, security)
    }
}
