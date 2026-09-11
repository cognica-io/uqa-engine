//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind namespace execution to current authorization state and catalog publication guards.
use super::MutationCoordinator;
use crate::Engine;
use uqa_execution::schema::namespaces::{
    privileges::{
        SchemaPrivilegeContext, SchemaPrivilegeNotices, SchemaPrivilegeRegistry, SchemaRegistryRead,
    },
    NamespaceCatalogChanges, NamespaceCatalogRefresh, SchemaAuthority, SchemaCreationContext,
    SchemaOwnerContext, SchemaRegistration, SchemaRegistrationPersistence, SchemaRegistrationState,
    SchemaRegistryWrite, SchemaSecurityCatalog, SchemaSecurityPersistence,
    SchemaSecurityPublication, SchemaStatementWriter,
};
use uqa_sql::{catalog::security::SchemaSecurity, SQLError};
use uqa_storage::StorageBackendResult;

impl Engine {
    pub(crate) fn schema_creation_context(&self) -> SchemaCreationContext<'_> {
        SchemaCreationContext {
            writer: self,
            session: self,
            authority: self,
            registration: self,
        }
    }
    pub(crate) fn schema_owner_context(&self) -> SchemaOwnerContext<'_> {
        SchemaOwnerContext {
            writer: self,
            refresh: self,
            session: self,
            roles: self,
            authority: self,
            catalog: self,
            persistence: self,
            publication: self,
            changes: self,
        }
    }
    pub(crate) fn schema_privilege_context(&self) -> SchemaPrivilegeContext<'_> {
        SchemaPrivilegeContext {
            writer: self,
            refresh: self,
            session: self,
            roles: self,
            registry: self,
            persistence: self,
            notices: self,
            changes: self,
        }
    }
}
impl SchemaStatementWriter for Engine {
    fn prepare_writer(&self) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer().map(|_| ())
    }
}
impl NamespaceCatalogRefresh for Engine {
    fn refresh_catalog(&self) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()
    }
}
impl SchemaAuthority for Engine {
    fn current_user_has_role_privileges(&self, role: &str) -> bool {
        Engine::current_user_has_role_privileges(self, role)
    }
    fn current_user_is_superuser(&self) -> bool {
        Engine::current_user_is_superuser(self)
    }
    fn ensure_database_create(&self, role: &str) -> Result<(), SQLError> {
        self.ensure_database_privilege(role, crate::database_security::DatabaseAclPrivilege::Create)
    }
}
impl SchemaRegistrationState for MutationCoordinator<'_> {
    fn schemas_write(&self) -> SchemaRegistryWrite<'_> {
        Box::new(self.durable.schemas.write())
    }
    fn contains_graph(&self, name: &str) -> bool {
        self.durable.graphs.read().contains_key(name)
    }
}
impl SchemaRegistrationPersistence for MutationCoordinator<'_> {
    fn persist_schema(&self, name: &str, security: &SchemaSecurity) -> StorageBackendResult<()> {
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.save_schema_row(&security.row(name))?;
        }
        Ok(())
    }
}
impl NamespaceCatalogChanges for MutationCoordinator<'_> {
    fn catalog_registry_changed(&self) {
        self.note_catalog_registry_changed();
    }
}
impl NamespaceCatalogChanges for Engine {
    fn catalog_registry_changed(&self) {
        self.note_catalog_registry_changed();
    }
}
impl SchemaRegistration for Engine {
    fn register_schema(
        &self,
        name: &str,
        if_not_exists: bool,
        role_owner: &str,
    ) -> StorageBackendResult<bool> {
        self.mutation_coordinator()
            .register_schema(name, if_not_exists, role_owner)
    }
}
impl SchemaSecurityCatalog for Engine {
    fn schema_security(&self, name: &str) -> Option<SchemaSecurity> {
        self.schema_security_for_privilege(name)
    }
}
impl SchemaSecurityPersistence for Engine {
    fn persist_security(&self, name: &str, security: &SchemaSecurity) -> Result<(), SQLError> {
        self.persist_schema_security(name, security)
    }
}
impl SchemaSecurityPublication for Engine {
    fn publish_security(&self, name: &str, security: SchemaSecurity) {
        self.durable
            .schemas
            .write()
            .insert(name.to_string(), security);
    }
}
impl SchemaPrivilegeRegistry for Engine {
    fn schemas_read(&self) -> SchemaRegistryRead<'_> {
        Box::new(self.durable.schemas.read())
    }
    fn schemas_write(&self) -> SchemaRegistryWrite<'_> {
        Box::new(self.durable.schemas.write())
    }
}
impl SchemaPrivilegeNotices for Engine {
    fn schema_privilege_notice(&self, level: &str, message: &str) {
        self.push_sql_notice(level, message);
    }
}
