//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{Engine, TableState};
use parking_lot::RwLockReadGuard;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::RelationIdentity;
use uqa_execution::catalog::security::table_inquiry::{
    PrivilegeForeignSecurityRead, PrivilegeForeignTablesRead, PrivilegeViewsRead, TableColumnsRead,
    TablePrivilegeRead, TablePrivilegeRegistry, TablePrivilegeState,
};
use uqa_sql::catalog::security::{table::TableAclPrivilege, TableSecurity};
use uqa_storage::StorageBackendResult;

struct ObservedTable {
    state: Arc<TableState>,
    columns_reads: Arc<AtomicUsize>,
}
impl TablePrivilegeState for ObservedTable {
    fn role_owner(&self) -> String {
        self.state.role_owner()
    }
    fn security(&self) -> TableSecurity {
        self.state.security()
    }
    fn column_names(&self) -> Vec<String> {
        TablePrivilegeState::column_names(self.state.as_ref())
    }
    fn columns(&self) -> TableColumnsRead<'_> {
        self.columns_reads.fetch_add(1, Ordering::SeqCst);
        Box::new(self.state.columns.read())
    }
}
struct ObservedRead<'a> {
    actual: RwLockReadGuard<'a, BTreeMap<RelationIdentity, Arc<TableState>>>,
    columns_reads: Arc<AtomicUsize>,
}
impl TablePrivilegeRead for ObservedRead<'_> {
    fn keys(&self) -> Box<dyn Iterator<Item = &RelationIdentity> + '_> {
        Box::new(self.actual.keys())
    }
    fn security_entries(&self) -> Box<dyn Iterator<Item = (RelationIdentity, TableSecurity)> + '_> {
        Box::new(
            self.actual
                .iter()
                .map(|(relation, table)| (relation.clone(), table.security())),
        )
    }
    fn get(&self, relation: &RelationIdentity) -> Option<&dyn TablePrivilegeState> {
        self.actual
            .get(relation)
            .map(|state| state.as_ref() as &dyn TablePrivilegeState)
    }
    fn retained(&self, relation: &RelationIdentity) -> Option<Arc<dyn TablePrivilegeState>> {
        self.actual.get(relation).cloned().map(|state| {
            Arc::new(ObservedTable {
                state,
                columns_reads: Arc::clone(&self.columns_reads),
            }) as Arc<dyn TablePrivilegeState>
        })
    }
}
struct ObservedRegistry<'a> {
    engine: &'a Engine,
    columns_reads: Arc<AtomicUsize>,
}
impl TablePrivilegeRegistry for ObservedRegistry<'_> {
    fn refresh_tables(&self) -> StorageBackendResult<()> {
        TablePrivilegeRegistry::refresh_tables(self.engine)
    }
    fn refresh_catalog(&self) -> StorageBackendResult<()> {
        TablePrivilegeRegistry::refresh_catalog(self.engine)
    }
    fn tables(&self) -> Box<dyn TablePrivilegeRead + '_> {
        Box::new(ObservedRead {
            actual: self.engine.storage.tables.read(),
            columns_reads: Arc::clone(&self.columns_reads),
        })
    }
    fn views(&self) -> PrivilegeViewsRead<'_> {
        TablePrivilegeRegistry::views(self.engine)
    }
    fn foreign_tables(&self) -> PrivilegeForeignTablesRead<'_> {
        TablePrivilegeRegistry::foreign_tables(self.engine)
    }
    fn foreign_security(&self) -> PrivilegeForeignSecurityRead<'_> {
        TablePrivilegeRegistry::foreign_security(self.engine)
    }
}

#[test]
fn bound_authorization_retains_the_selected_table_generation_after_registry_replacement() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE original(id integer); CREATE TABLE replacement(changed text)",
            &[],
        )
        .unwrap();
    let name = RelationIdentity::new("public", "original");
    let (_, selected) = engine
        .table_authorization_context()
        .bound_table_for_security("public.original")
        .unwrap();
    let replacement =
        engine.storage.tables.read()[&RelationIdentity::new("public", "replacement")].clone();
    engine
        .storage
        .tables
        .try_write()
        .expect("binding must release the actual registry guard")
        .insert(name, replacement);
    assert_eq!(selected.column_names(), ["id"]);
    assert_eq!(
        engine.bound_table_column_names("public.original").unwrap(),
        ["changed"]
    );
}

#[test]
fn any_column_authority_opens_the_actual_column_guard_only_after_table_privilege_fails() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE reader; CREATE TABLE items(id integer, hidden text); GRANT SELECT(id) ON items TO reader",&[]).unwrap();
    let registry = ObservedRegistry {
        engine: &engine,
        columns_reads: Arc::new(AtomicUsize::new(0)),
    };
    let mut context = engine.table_authorization_context();
    context.registry = &registry;
    context
        .ensure_any_column_privilege_for("public.items", "uqa", TableAclPrivilege::Select)
        .unwrap();
    assert_eq!(registry.columns_reads.load(Ordering::SeqCst), 0);
    context
        .ensure_any_column_privilege_for("public.items", "reader", TableAclPrivilege::Select)
        .unwrap();
    assert_eq!(registry.columns_reads.load(Ordering::SeqCst), 1);
    let error = context
        .ensure_any_column_privilege_for("public.items", "reader", TableAclPrivilege::Insert)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(registry.columns_reads.load(Ordering::SeqCst), 2);
}

#[test]
fn schema_owner_can_drop_table_and_foreign_table_without_gaining_data_or_owner_privileges() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE custodian; CREATE SCHEMA tenant AUTHORIZATION custodian; CREATE TABLE tenant.items(id integer); CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE tenant.remote(id integer) SERVER source; SET ROLE custodian",&[]).unwrap();
    engine.ensure_table_drop_authority("tenant.items").unwrap();
    engine
        .ensure_foreign_table_drop_authority("tenant.remote")
        .unwrap();
    for error in [
        engine.ensure_table_owner("tenant.items").unwrap_err(),
        engine
            .ensure_foreign_table_owner("tenant.remote")
            .unwrap_err(),
        engine
            .ensure_table_privilege("tenant.items", TableAclPrivilege::Select)
            .unwrap_err(),
        engine
            .ensure_foreign_table_privilege("tenant.remote", TableAclPrivilege::Select)
            .unwrap_err(),
    ] {
        assert_eq!(error.sqlstate(), Some("42501"));
    }
    engine
        .sql(
            "DROP TABLE tenant.items; DROP FOREIGN TABLE tenant.remote",
            &[],
        )
        .unwrap();
    assert!(!engine.try_has_table("tenant.items").unwrap());
    assert!(engine.foreign_table("tenant.remote").unwrap().is_none());
}
