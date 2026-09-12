//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain table generations and lend live catalog guards to native GRANT execution.
use crate::{Engine, TableState};
use parking_lot::RwLockReadGuard;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_execution::catalog::security::{
    table_grants::context::{
        TableGrantContext, TableGrantInputs, TableGrantNotices, TableGrantRead, TableGrantRegistry,
        TableGrantState, TableSecurityWrite,
    },
    table_inquiry::TablePrivilegeState,
};
use uqa_sql::{
    catalog::{
        resolution::RelationResolution,
        security::{table_grants::targets::TableGrantResolution, TableSecurity},
    },
    SQLError,
};
use uqa_storage::StorageBackendResult;
struct GrantTable<'a> {
    engine: &'a Engine,
    state: Arc<TableState>,
}
struct GrantTables<'a> {
    engine: &'a Engine,
    guard: RwLockReadGuard<'a, BTreeMap<RelationIdentity, Arc<TableState>>>,
}
impl TableGrantRegistry for Engine {
    fn tables(&self) -> Box<dyn TableGrantRead<'_> + '_> {
        Box::new(GrantTables {
            engine: self,
            guard: self.storage.tables.read(),
        })
    }
}
impl<'a> TableGrantRead<'a> for GrantTables<'a> {
    fn keys(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_> {
        Box::new(self.guard.keys())
    }
    fn retained(&self, relation: &RelationIdentity) -> Option<Box<dyn TableGrantState + 'a>> {
        self.guard.get(relation).cloned().map(|state| {
            Box::new(GrantTable {
                engine: self.engine,
                state,
            }) as Box<dyn TableGrantState + 'a>
        })
    }
}
impl TablePrivilegeState for GrantTable<'_> {
    fn role_owner(&self) -> String {
        self.state.role_owner()
    }
    fn columns(&self) -> uqa_execution::catalog::security::table_inquiry::TableColumnsRead<'_> {
        Box::new(self.state.columns.read())
    }
    fn security(&self) -> TableSecurity {
        self.state.security()
    }
    fn column_names(&self) -> Vec<String> {
        TablePrivilegeState::column_names(self.state.as_ref())
    }
}
impl TableGrantState for GrantTable<'_> {
    fn security_write(&self) -> TableSecurityWrite<'_> {
        Box::new(self.state.security.write())
    }
    fn persist_security(&self, name: &str, security: &TableSecurity) -> StorageBackendResult<()> {
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
        self.engine
            .try_save_table_schema_with_components_and_security(
                name,
                table,
                &columns,
                &constraints,
                security,
            )
    }
}
impl TableGrantResolution for Engine {
    fn resolve_visible_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError> {
        Engine::resolve_visible_relation_kind(self, name)
    }
}
impl TableGrantNotices for Engine {
    fn notice(&self, level: &str, message: &str) {
        self.push_sql_notice(level, message);
    }
}
impl TableGrantInputs for Engine {
    fn table_grant_context(&self) -> TableGrantContext<'_> {
        TableGrantContext {
            writer: self,
            resolution: self,
            namespaces: self,
            names: self,
            roles: self,
            registry: self,
            tables: self,
            views: self,
            foreign: self,
            catalog: self.storage.catalog.as_deref(),
            changes: self,
            notices: self,
            sequences: self.sequence_privilege_context(),
        }
    }
}
