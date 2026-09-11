//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend relation registries and retained table generations to privilege inquiry.

use crate::{Engine, TableState};
use parking_lot::RwLockReadGuard;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_execution::catalog::security::table_inquiry::{
    PrivilegeForeignSecurityRead, PrivilegeForeignTablesRead, PrivilegeViewsRead,
    TablePrivilegeContext, TablePrivilegeRead, TablePrivilegeRegistry, TablePrivilegeState,
};
use uqa_sql::catalog::security::TableSecurity;
use uqa_storage::StorageBackendResult;

struct TablePrivilegeGuard<'a>(RwLockReadGuard<'a, BTreeMap<RelationIdentity, Arc<TableState>>>);
impl TablePrivilegeRead for TablePrivilegeGuard<'_> {
    fn keys(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_> {
        Box::new(self.0.keys())
    }
    fn get(&self, relation: &RelationIdentity) -> Option<&dyn TablePrivilegeState> {
        self.0
            .get(relation)
            .map(|table| table.as_ref() as &dyn TablePrivilegeState)
    }
    fn retained(&self, relation: &RelationIdentity) -> Option<Arc<dyn TablePrivilegeState>> {
        self.0
            .get(relation)
            .cloned()
            .map(|table| table as Arc<dyn TablePrivilegeState>)
    }
}
impl TablePrivilegeState for TableState {
    fn security(&self) -> TableSecurity {
        self.security()
    }
    fn column_names(&self) -> Vec<String> {
        self.columns
            .read()
            .iter()
            .map(|column| column.name.clone())
            .collect()
    }
}
impl TablePrivilegeRegistry for Engine {
    fn refresh_tables(&self) -> StorageBackendResult<()> {
        self.synchronize_table_catalog()
    }
    fn refresh_catalog(&self) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()
    }
    fn tables(&self) -> Box<dyn TablePrivilegeRead + '_> {
        Box::new(TablePrivilegeGuard(self.storage.tables.read()))
    }
    fn views(&self) -> PrivilegeViewsRead<'_> {
        Box::new(self.durable.views.read())
    }
    fn foreign_tables(&self) -> PrivilegeForeignTablesRead<'_> {
        Box::new(self.durable.foreign_tables.read())
    }
    fn foreign_security(&self) -> PrivilegeForeignSecurityRead<'_> {
        Box::new(self.durable.foreign_table_security.read())
    }
}
impl Engine {
    pub(crate) fn table_privilege_context(&self) -> TablePrivilegeContext<'_> {
        TablePrivilegeContext {
            names: self,
            roles: self,
            sequences: self.sequence_privilege_inquiry(),
            catalog: self.catalog_execution(),
            registry: self,
        }
    }
}
