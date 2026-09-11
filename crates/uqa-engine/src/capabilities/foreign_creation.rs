//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind foreign declaration execution to retained registries, fresh catalog scopes and the session transaction.
use crate::Engine;
use uqa_execution::schema::foreign_creation::{
    entry::{ForeignCreationTransactions, ForeignServerWrite, ForeignTableWrite},
    ForeignCreationContext, ForeignCreationNamespace, ForeignCreationRegistry, ForeignSecurityRead,
    ForeignServersRead, ForeignServersWrite, ForeignTablesRead,
};
use uqa_sql::{schema::foreign_tables::ForeignSchemaContext, SQLError};
use uqa_storage::StorageBackendResult;
impl Engine {
    pub(crate) fn foreign_schema_context(&self) -> ForeignSchemaContext<'_> {
        ForeignSchemaContext {
            types: self,
            schema: self,
            bindings: self,
            references: self,
            sequences: self,
            allocate_identity: super::allocate_catalog_object_id,
        }
    }
    pub(crate) fn foreign_creation_context(&self) -> ForeignCreationContext<'_> {
        ForeignCreationContext {
            schema: self.foreign_schema_context(),
            namespace: self,
            registry: self,
            publication: self,
            catalog: self.storage.catalog.as_deref(),
            changes: self,
            session: self,
            sequences: self.implicit_sequence_context(),
            ownership: self.implicit_ownership_context(),
            notices: self.query_runtime_view().notices,
            allocate_identity: crate::new_table_object_id,
        }
    }
}
impl ForeignRegistryReads for Engine {
    fn servers(&self) -> ForeignServersRead<'_> {
        Box::new(self.durable.foreign_servers.read())
    }
    fn tables(&self) -> ForeignTablesRead<'_> {
        Box::new(self.durable.foreign_tables.read())
    }
    fn security(&self) -> ForeignSecurityRead<'_> {
        Box::new(self.durable.foreign_table_security.read())
    }
}
impl ForeignCreationRegistry for Engine {
    fn servers_write(&self) -> ForeignServersWrite<'_> {
        Box::new(self.durable.foreign_servers.write())
    }
}
impl ForeignCreationNamespace for Engine {
    fn synchronize_catalog_registries(&self) -> StorageBackendResult<()> {
        Engine::synchronize_catalog_registries(self)
    }
    fn relation_name_for_create(&self, name: &str) -> Result<String, SQLError> {
        self.try_relation_name_for_sql_create(name)
    }
    fn relation_kind_at(&self, name: &str) -> StorageBackendResult<Option<&'static str>> {
        Engine::relation_kind_at(self, name)
    }
}
impl ForeignCreationTransactions for Engine {
    fn with_foreign_server_write(&self, write: ForeignServerWrite<'_>) -> Result<(), String> {
        self.with_implicit_string_transaction(|engine| write(&engine.foreign_creation_context()))
    }
    fn with_foreign_table_write(&self, write: ForeignTableWrite<'_>) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.foreign_creation_context()))
    }
}

use uqa_execution::catalog::foreign::reads::ForeignRegistryReads;
