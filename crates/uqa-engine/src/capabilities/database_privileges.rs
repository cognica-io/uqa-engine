//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind database privilege inquiry, publication and restoration to live catalog state.

use crate::Engine;
use uqa_core::Value;
use uqa_sql::{
    catalog::security::{
        database::DatabaseAclPrivilege,
        database_inquiry::{
            DatabasePrivilegeCatalog, DatabasePrivilegeInquiry, DatabaseSecurityRead,
        },
    },
    SQLError,
};

impl DatabasePrivilegeCatalog for Engine {
    fn refresh_privilege_catalog(&self) -> Result<(), SQLError> {
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!("load database privileges for inquiry: {error}"))
        })
    }
    fn security(&self) -> DatabaseSecurityRead<'_> {
        Box::new(self.durable.database_security.read())
    }
}
impl Engine {
    pub(crate) fn database_privilege_inquiry(&self) -> DatabasePrivilegeInquiry<'_> {
        DatabasePrivilegeInquiry {
            catalog: self,
            names: self,
            roles: self,
        }
    }
    pub(crate) fn has_database_privilege_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        self.database_privilege_inquiry()
            .has_database_privilege_value(arguments)
    }
    pub(crate) fn ensure_database_privilege(
        &self,
        role: &str,
        privilege: DatabaseAclPrivilege,
    ) -> Result<(), SQLError> {
        self.database_privilege_inquiry()
            .ensure_database_privilege(role, privilege)
    }
}

use uqa_execution::catalog::security::database_lifecycle::{
    self, DatabasePrivilegeContext, DatabasePrivilegePublication, DatabaseSecurityRegistry,
    DatabaseSecurityWrite, DATABASE_SECURITY_METADATA_KEY,
};
use uqa_sql::ast::GrantDatabaseStmt;
use uqa_sql::catalog::security::database::DatabaseSecurity;
use uqa_storage::{CatalogFacade, StorageBackendResult};

impl DatabaseSecurityRegistry for Engine {
    fn security_read(&self) -> DatabaseSecurityRead<'_> {
        Box::new(self.durable.database_security.read())
    }
    fn security_write(&self) -> DatabaseSecurityWrite<'_> {
        Box::new(self.durable.database_security.write())
    }
}
impl DatabasePrivilegePublication for Engine {
    fn prepare_writer(&self) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer().map(|_| ())
    }
    fn refresh_catalog(&self) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()
    }
    fn persist_security(&self, security: &DatabaseSecurity) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        let json = serde_json::to_string(security).map_err(|error| {
            SQLError::Internal(format!("serialize database privileges: {error}"))
        })?;
        catalog
            .set_metadata(DATABASE_SECURITY_METADATA_KEY, &json)
            .map_err(|error| SQLError::Internal(format!("persist database privileges: {error}")))
    }

    fn catalog_changed(&self) {
        self.note_catalog_registry_changed();
    }
    fn notice(&self, level: &str, message: &str) {
        self.push_sql_notice(level, message);
    }
}
impl Engine {
    pub(crate) fn database_privilege_context(&self) -> DatabasePrivilegeContext<'_> {
        DatabasePrivilegeContext {
            names: self,
            roles: self,
            registry: self,
            publication: self,
        }
    }
    pub(crate) fn grant_database_privileges(
        &self,
        statement: &GrantDatabaseStmt,
    ) -> Result<(), SQLError> {
        database_lifecycle::grant_database_privileges(&self.database_privilege_context(), statement)
    }
    pub(crate) fn restore_database_security_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        database_lifecycle::restore_database_security_from_metadata(
            &self.database_privilege_context(),
            catalog,
        )
    }
}
