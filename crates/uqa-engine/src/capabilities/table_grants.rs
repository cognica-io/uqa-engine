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
        TableGrantContext, TableGrantInputs, TableGrantNotices, TableGrantPersistence,
        TableGrantRead, TableGrantRegistry, TableGrantState, TableSecurityWrite,
    },
    table_inquiry::TablePrivilegeState,
};
use uqa_sql::{
    catalog::{
        resolution::RelationResolution,
        security::{table_grants::targets::TableGrantResolution, BoundTableSecurity},
    },
    SQLError,
};
struct GrantTable {
    state: Arc<TableState>,
}
struct GrantTables<'a> {
    guard: RwLockReadGuard<'a, BTreeMap<RelationIdentity, Arc<TableState>>>,
}
impl TableGrantRegistry for Engine {
    fn tables(&self) -> Box<dyn TableGrantRead<'_> + '_> {
        Box::new(GrantTables {
            guard: self.storage.tables.read(),
        })
    }
}
impl<'a> TableGrantRead<'a> for GrantTables<'a> {
    fn keys(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_> {
        Box::new(self.guard.keys())
    }
    fn retained(&self, relation: &RelationIdentity) -> Option<Box<dyn TableGrantState + 'a>> {
        self.guard
            .get(relation)
            .cloned()
            .map(|state| Box::new(GrantTable { state }) as Box<dyn TableGrantState + 'a>)
    }
}
impl TablePrivilegeState for GrantTable {
    fn role_owner(&self) -> uqa_sql::catalog::roles::RoleIdentity {
        self.state.role_owner()
    }
    fn columns(&self) -> uqa_execution::catalog::security::table_inquiry::TableColumnsRead<'_> {
        Box::new(self.state.columns.read())
    }
    fn security(&self) -> BoundTableSecurity {
        self.state.security()
    }
    fn column_names(&self) -> Vec<String> {
        TablePrivilegeState::column_names(self.state.as_ref())
    }
}
impl TableGrantState for GrantTable {
    fn security_write(&self) -> TableSecurityWrite<'_> {
        Box::new(self.state.security.write())
    }
    fn persistence(&self) -> uqa_sql::ast::RelationPersistence {
        self.state.persistence
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
impl TableGrantPersistence for Engine {
    fn persist_relation_acl(
        &self,
        relation: &RelationIdentity,
        column: Option<&str>,
        entry: &uqa_storage::catalog::relation_acl::RelationAclTuple,
    ) -> uqa_storage::StorageBackendResult<()> {
        if let Some(catalog) = &self.storage.catalog {
            catalog.save_relation_acl(relation, column, entry)?;
        }
        Ok(())
    }
}
impl TableGrantInputs for Engine {
    fn table_grant_context(&self) -> TableGrantContext<'_> {
        TableGrantContext {
            writer: self,
            bindings: self,
            locks: self,
            shared_locks: self,
            rows: self,
            system: self,
            resolution: self,
            namespaces: self,
            names: self,
            roles: self,
            registry: self,
            tables: self,
            views: self,
            foreign: self,
            acls: self,
            catalog: self.storage.catalog.as_deref(),
            changes: self,
            notices: self,
            sequences: self.sequence_privilege_context(),
        }
    }
}
