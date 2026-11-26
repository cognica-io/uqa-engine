//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind database privileges and database-local default ACLs to session and catalog state.

use crate::Engine;
use uqa_core::catalog_role::RoleIdentity;
use uqa_execution::catalog::security::{
    default_acl::{
        DefaultAclContext, DefaultAclPublication, DefaultAclWrite, DefaultPrivilegeWrite,
    },
    roles::locking::RoleLockContext,
};
use uqa_sql::catalog::roles::identity::RoleSubject;
use uqa_sql::{
    ast::DefaultAclObject,
    catalog::security::default_acl::{DefaultAcls, Entries, InitialPrivileges},
};
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
        Box::new(parking_lot::MappedRwLockReadGuard::map(
            self.durable.database_security.read(),
            AsRef::as_ref,
        ))
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
    pub(crate) fn ensure_database_privilege(
        &self,
        role: &(impl RoleSubject + ?Sized),
        privilege: DatabaseAclPrivilege,
    ) -> Result<(), SQLError> {
        self.database_privilege_inquiry()
            .ensure_database_privilege(role, privilege)
    }
}

use uqa_execution::catalog::security::database_lifecycle::{
    self, DatabaseAuthorityRead, DatabasePrivilegeContext, DatabasePrivilegePublication,
    DatabaseSecurityRegistry, DatabaseSecurityWrite, DATABASE_SECURITY_METADATA_KEY,
};
use uqa_storage::{CatalogFacade, StorageBackendResult};

impl DatabaseSecurityRegistry for Engine {
    fn security_read(&self) -> DatabaseAuthorityRead<'_> {
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
    fn persist_security(&self, json: &str) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
            return Ok(());
        };
        catalog
            .set_metadata(DATABASE_SECURITY_METADATA_KEY, json)
            .map_err(|error| SQLError::Internal(format!("persist database privileges: {error}")))
    }

    fn catalog_changed(&self) {
        self.note_catalog_registry_changed();
    }
    fn notice(&self, notice: uqa_sql::SQLNotice) {
        self.push_sql_notice(notice);
    }
}
impl Engine {
    pub(crate) fn database_privilege_context(&self) -> DatabasePrivilegeContext<'_> {
        DatabasePrivilegeContext {
            locks: self,
            names: self,
            roles: self,
            registry: self,
            publication: self,
        }
    }
    pub(crate) fn restore_database_security_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
        allow_migration: bool,
    ) -> StorageBackendResult<()> {
        database_lifecycle::restore_database_security_from_metadata(
            &self.database_privilege_context(),
            catalog,
            allow_migration,
        )
    }
}

impl InitialPrivileges for Engine {
    fn initial_acl(
        &self,
        owner: RoleIdentity,
        namespace: Option<&str>,
        object: DefaultAclObject,
    ) -> Option<Entries> {
        let defaults = self.durable.default_acls.snapshot();
        if defaults.is_empty() {
            return None;
        }
        let oid = namespace
            .and_then(|name| {
                self.schema_privilege_inquiry()
                    .schema_security_for_privilege(name)
                    .map(|security| security.namespace_oid(name))
            })
            .and_then(|oid| u32::try_from(oid).ok())
            .unwrap_or(0);
        defaults.initial(owner, oid, object)
    }
}

impl DefaultAclPublication for Engine {
    fn default_acls(&self) -> DefaultAcls {
        self.durable.default_acls.read().clone()
    }
    fn default_acl_catalog(&self) -> Option<&dyn uqa_storage::CatalogFacade> {
        self.storage.catalog.as_deref()
    }
    fn prepare_default_acl_writer(&self) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer().map(|_| ())
    }
    fn publish_default_acls(&self, next: DefaultAcls) {
        *self.durable.default_acls.write() = next;
        self.note_catalog_registry_changed();
    }
}

impl DefaultPrivilegeWrite for Engine {
    fn with_default_acl_write(&self, write: DefaultAclWrite<'_>) -> Result<(), SQLError> {
        self.with_implicit_definition_transaction(|engine| {
            write(&DefaultAclContext {
                publication: engine,
                roles: RoleLockContext {
                    roles: engine,
                    session: engine,
                },
                schemas: engine.schema_privilege_inquiry(),
            })
        })
    }
}
