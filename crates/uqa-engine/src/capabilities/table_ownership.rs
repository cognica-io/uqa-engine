//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the selected table generation and lend schema/security publication state to ownership execution.
use crate::{Engine, TableState};
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_execution::{
    catalog::security::{
        table_grants::context::TableSecurityWrite,
        table_inquiry::{TableColumnsRead, TablePrivilegeState},
        table_ownership::{
            TableOwnerPublication, TableOwnerRegistry, TableOwnerSchema, TableOwnerState,
            TableOwnershipContext,
        },
    },
    schema::sequences::role_ownership::OwnedSequenceSecurityWrite,
};
use uqa_sql::{ast::RelationPersistence, catalog::security::TableSecurity};
use uqa_storage::StorageBackendResult;
struct OwnedTable<'a> {
    engine: &'a Engine,
    state: Arc<TableState>,
}
impl TableOwnerRegistry for Engine {
    fn table(&self, relation: &RelationIdentity) -> Option<Box<dyn TableOwnerState + '_>> {
        self.storage
            .tables
            .read()
            .get(relation)
            .cloned()
            .map(|state| {
                Box::new(OwnedTable {
                    engine: self,
                    state,
                }) as Box<dyn TableOwnerState>
            })
    }
}
impl TablePrivilegeState for OwnedTable<'_> {
    fn role_owner(&self) -> String {
        self.state.role_owner()
    }
    fn security(&self) -> TableSecurity {
        self.state.security()
    }
    fn columns(&self) -> TableColumnsRead<'_> {
        Box::new(self.state.columns.read())
    }
    fn column_names(&self) -> Vec<String> {
        TablePrivilegeState::column_names(self.state.as_ref())
    }
}
impl TableOwnerState for OwnedTable<'_> {
    fn object_id(&self) -> [u8; 16] {
        self.state.object_id()
    }
    fn persistence(&self) -> RelationPersistence {
        self.state.persistence
    }
    fn schema(&self) -> TableOwnerSchema {
        let table = self.state.as_ref();
        let columns = table.columns.read().clone();
        let constraints = uqa_sql::ast::TableConstraintSet {
            columns_declared: Some(*table.columns_declared.read()),
            checks: table.table_checks.read().clone(),
            foreign_keys: table.foreign_keys.read().clone(),
            key_constraints: table.key_constraints.read().clone(),
            persistence: table.persistence,
            on_commit: table.on_commit,
            hierarchy: table.hierarchy.read().clone(),
        };
        TableOwnerSchema {
            columns,
            constraints,
        }
    }
    fn persist_schema(
        &self,
        name: &str,
        schema: &TableOwnerSchema,
        security: &TableSecurity,
    ) -> StorageBackendResult<()> {
        self.engine
            .try_save_table_schema_with_components_and_security(
                name,
                self.state.as_ref(),
                &schema.columns,
                &schema.constraints,
                security,
            )
    }
    fn security_write(&self) -> TableSecurityWrite<'_> {
        Box::new(self.state.security.write())
    }
}
impl TableOwnerPublication for Engine {
    fn sequence_security_write(&self) -> OwnedSequenceSecurityWrite<'_> {
        Box::new(self.durable.sequence_security.write())
    }
}
impl Engine {
    pub(crate) fn table_ownership_context(&self) -> TableOwnershipContext<'_> {
        TableOwnershipContext {
            writer: self,
            tables: self,
            authorization: self.table_authorization_context(),
            roles: self.role_transfer_context(),
            owned_sequences: self,
            sequences: self,
            publication: self,
            changes: self,
        }
    }
}

#[cfg(test)]
mod tests;
