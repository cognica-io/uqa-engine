//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind read-only table and view authorization to the active Engine catalogs.
use crate::Engine;
use uqa_execution::catalog::security::{
    foreign_authorization::ForeignAuthorizationContext,
    table_authorization::TableAuthorizationContext, table_maintenance::TableMaintenanceContext,
};
use uqa_sql::{
    catalog::security::{table::TableAclPrivilege, view_authorization::ViewAuthorizationContext},
    SQLError,
};
impl Engine {
    pub(crate) fn table_authorization_context(&self) -> TableAuthorizationContext<'_> {
        TableAuthorizationContext {
            names: self,
            roles: self,
            registry: self,
            schemas: self,
        }
    }
    pub(crate) fn foreign_authorization_context(&self) -> ForeignAuthorizationContext<'_> {
        ForeignAuthorizationContext {
            names: self,
            roles: self,
            schemas: self,
            catalog: self,
        }
    }
    pub(crate) fn ensure_table_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.table_authorization_context()
            .ensure_table_privilege(name, privilege)
    }
    pub(crate) fn ensure_table_privilege_for(
        &self,
        name: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.table_authorization_context()
            .ensure_table_privilege_for(name, subject, privilege)
    }
    pub(crate) fn bound_table_column_names(&self, name: &str) -> Result<Vec<String>, SQLError> {
        self.table_authorization_context()
            .bound_table_column_names(name)
    }
    pub(crate) fn ensure_column_privilege(
        &self,
        name: &str,
        column: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.table_authorization_context()
            .ensure_column_privilege(name, column, privilege)
    }
    pub(crate) fn ensure_column_privilege_for(
        &self,
        name: &str,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.table_authorization_context()
            .ensure_column_privilege_for(name, column, subject, privilege)
    }
    pub(crate) fn ensure_any_column_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.table_authorization_context()
            .ensure_any_column_privilege(name, privilege)
    }
    pub(crate) fn ensure_any_column_privilege_for(
        &self,
        name: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.table_authorization_context()
            .ensure_any_column_privilege_for(name, subject, privilege)
    }
    pub(crate) fn ensure_table_owner(&self, name: &str) -> Result<String, SQLError> {
        self.table_authorization_context().ensure_table_owner(name)
    }
    pub(crate) fn ensure_table_drop_authority(&self, name: &str) -> Result<(), SQLError> {
        self.table_authorization_context()
            .ensure_table_drop_authority(name)
    }
    pub(crate) fn ensure_view_privilege_for(
        &self,
        name: &str,
        view: &crate::StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        ViewAuthorizationContext { roles: self }
            .ensure_view_privilege_for(name, view, subject, privilege)
    }
    pub(crate) fn ensure_view_column_privilege_for(
        &self,
        name: &str,
        view: &crate::StoredView,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        ViewAuthorizationContext { roles: self }
            .ensure_view_column_privilege_for(name, view, column, subject, privilege)
    }
    pub(crate) fn ensure_any_view_column_privilege_for(
        &self,
        name: &str,
        view: &crate::StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        ViewAuthorizationContext { roles: self }
            .ensure_any_view_column_privilege_for(name, view, subject, privilege)
    }
    pub(crate) fn maintenance_table_names(&self, operation: &str) -> Result<Vec<String>, SQLError> {
        TableMaintenanceContext {
            authorization: self.table_authorization_context(),
            notices: self,
        }
        .maintenance_table_names(operation)
    }
    pub(crate) fn ensure_foreign_table_owner(&self, name: &str) -> Result<String, SQLError> {
        self.foreign_authorization_context()
            .ensure_foreign_table_owner(name)
    }
    pub(crate) fn ensure_foreign_table_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.foreign_authorization_context()
            .ensure_foreign_table_privilege(name, privilege)
    }
    pub(crate) fn ensure_foreign_table_drop_authority(&self, name: &str) -> Result<(), SQLError> {
        self.foreign_authorization_context()
            .ensure_foreign_table_drop_authority(name)
    }
    pub(crate) fn persist_foreign_table_security(
        &self,
        relation: &uqa_core::RelationIdentity,
        security: &uqa_sql::catalog::security::TableSecurity,
    ) -> Result<(), SQLError> {
        uqa_execution::catalog::security::foreign_authorization::persist_foreign_table_security(
            self.storage.catalog.as_deref(),
            relation,
            security,
        )
    }
}
