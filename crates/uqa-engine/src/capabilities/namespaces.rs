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
    NamespaceCatalogChanges, NamespaceCatalogRefresh, SchemaCreationContext, SchemaOwnerContext,
    SchemaRegistration, SchemaRegistrationPersistence, SchemaRegistrationState,
    SchemaRegistryWrite, SchemaSecurityCatalog, SchemaSecurityPersistence,
    SchemaSecurityPublication, SchemaStatementWriter,
};
use uqa_sql::{catalog::security::BoundSchemaSecurity, SQLError};
use uqa_storage::StorageBackendResult;

impl Engine {
    fn schema_lock_context(
        &self,
    ) -> uqa_execution::schema::namespaces::locking::SchemaLockContext<'_> {
        uqa_execution::schema::namespaces::locking::SchemaLockContext {
            objects: self,
            relations: self,
            rows: self,
            catalog: self,
        }
    }
    pub(crate) fn schema_creation_context(&self) -> SchemaCreationContext<'_> {
        SchemaCreationContext {
            tuples: self.schema_lock_context(),
            writer: self,
            session: self,
            roles: self,
            catalog: self,
            schemas: self,
            notices: self,
            locks: self,
            database: self,
            registration: self,
        }
    }
    pub(crate) fn schema_owner_context(&self) -> SchemaOwnerContext<'_> {
        SchemaOwnerContext {
            tuples: self.schema_lock_context(),
            writer: self,
            refresh: self,
            session: self,
            roles: self,
            locks: self,
            database: self,
            catalog: self,
            publication: self,
            persistence: self,
            changes: self,
        }
    }
    pub(crate) fn schema_privilege_context(&self) -> SchemaPrivilegeContext<'_> {
        SchemaPrivilegeContext {
            tuples: self.schema_lock_context(),
            locks: self,
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
impl SchemaRegistrationState for MutationCoordinator<'_> {
    fn schemas_write(&self) -> SchemaRegistryWrite<'_> {
        Box::new(self.durable.schemas.write())
    }
    fn contains_graph(&self, name: &str) -> bool {
        self.durable.graphs.read().contains_key(name)
    }
}
impl SchemaRegistrationPersistence for MutationCoordinator<'_> {
    fn persist_schema(
        &self,
        name: &str,
        security: &BoundSchemaSecurity,
    ) -> StorageBackendResult<()> {
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.save_schema_row(&security.row(name).into())?;
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
        role_owner: uqa_core::catalog_role::RoleIdentity,
        tuple: uqa_core::catalog_schema::SchemaTupleIdentity,
    ) -> StorageBackendResult<bool> {
        self.mutation_coordinator()
            .register_schema(name, if_not_exists, role_owner, tuple)
    }
}
impl SchemaSecurityCatalog for Engine {
    fn schema_security(&self, name: &str) -> Option<BoundSchemaSecurity> {
        self.schema_security_for_privilege(name)
    }
}
impl SchemaSecurityPersistence for Engine {
    fn persist_security(&self, name: &str, security: &BoundSchemaSecurity) -> Result<(), SQLError> {
        self.persist_schema_security(name, security)
    }
}
impl SchemaSecurityPublication for Engine {
    fn publish_security(&self, name: &str, security: BoundSchemaSecurity) {
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

use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_execution::schema::namespaces::removal::{
    EmptySchemaRemovalContext, EmptySchemaRemovalPersistence, EmptySchemaRemovalState,
    SchemaDropNotices, SchemaRemovalContext, SchemaRemovalNames, SchemaRemovalPublication,
    SchemaRemovalViews, SchemaTypeRoutineRemoval,
};
use uqa_sql::schema::namespaces::removal::{EmptySchemaCatalog, SchemaDropCatalog};

impl Engine {
    pub(crate) fn schema_removal_context(&self) -> SchemaRemovalContext<'_> {
        SchemaRemovalContext {
            tuples: self.schema_lock_context(),
            refresh: self,
            catalog: self,
            names: self,
            types: self,
            tables: self.table_removal_context(),
            routines: self,
            events: self,
            foreign: self.foreign_removal_context(),
            views: self,
            sequences: self,
            locks: self,
            publication: self,
            notices: self,
        }
    }
    pub(crate) fn empty_schema_removal_context(&self) -> EmptySchemaRemovalContext<'_> {
        EmptySchemaRemovalContext {
            tuples: self.schema_lock_context(),
            refresh: self,
            catalog: self,
            state: self,
            persistence: self,
            changes: self,
        }
    }
}
impl EmptySchemaCatalog for Engine {
    fn schema_registered(&self, name: &str) -> bool {
        self.durable.schemas.read().contains_key(name)
    }
    fn schema_is_empty(&self, name: &str) -> bool {
        Engine::schema_is_empty(self, name)
    }
}
impl SchemaDropCatalog for Engine {
    fn schema_security(&self, name: &str) -> Option<BoundSchemaSecurity> {
        self.schema_security_for_privilege(name)
    }
    fn current_user_has_role_privileges(
        &self,
        role: &uqa_core::catalog_role::RoleIdentity,
    ) -> bool {
        Engine::current_user_has_role_privileges(self, role)
    }
    fn schema_is_graph(&self, name: &str) -> Result<bool, String> {
        self.has_graph_in_execution(name)
            .map_err(|error| error.to_string())
    }
}
impl SchemaRemovalNames for Engine {
    fn schema_tables(&self, schemas: &BTreeSet<String>) -> Vec<String> {
        self.storage
            .tables
            .read()
            .keys()
            .filter(|relation| schemas.contains(&relation.schema))
            .map(RelationIdentity::qualified_name)
            .collect()
    }
    fn schema_foreign_tables(&self, schemas: &BTreeSet<String>) -> Vec<String> {
        self.durable
            .foreign_tables
            .read()
            .keys()
            .filter(|relation| schemas.contains(&relation.schema))
            .map(RelationIdentity::qualified_name)
            .collect()
    }
    fn schema_views(&self, schemas: &BTreeSet<String>) -> Vec<String> {
        self.durable
            .views
            .read()
            .keys()
            .filter(|relation| schemas.contains(&relation.schema))
            .map(RelationIdentity::qualified_name)
            .collect()
    }
    fn schema_sequences(&self, schemas: &BTreeSet<String>) -> Vec<String> {
        self.durable
            .sequences
            .read()
            .keys()
            .filter(|relation| schemas.contains(&relation.schema))
            .map(RelationIdentity::qualified_name)
            .collect()
    }
    fn graph_tables(&self, schema: &str) -> StorageBackendResult<Vec<String>> {
        self.tables_in_schema(schema)
    }
}
impl SchemaTypeRoutineRemoval for Engine {
    fn drop_schema_types_and_routines(&self, schemas: &BTreeSet<String>) -> Result<(), SQLError> {
        Engine::drop_schema_types_and_routines(self, schemas)
    }
}
impl SchemaRemovalViews for Engine {
    fn drop_views_depending_on_relations(&self, names: &[String]) -> StorageBackendResult<()> {
        Engine::drop_views_depending_on_relations(self, names)
    }
    fn remaining_view_drop_targets(&self, names: &[String]) -> Result<Vec<String>, SQLError> {
        Engine::remaining_view_drop_targets(self, names)
    }
    fn cascade_view_closure(&self, names: Vec<String>) -> Result<Vec<String>, SQLError> {
        Engine::cascade_view_closure(self, names)
    }
    fn drop_views_inner(&self, names: &[String], cascade: bool) -> Result<(), SQLError> {
        Engine::drop_views_inner(self, names, cascade)
    }
}
impl SchemaRemovalPublication for Engine {
    fn drop_graph(&self, name: &str) -> StorageBackendResult<()> {
        Engine::drop_graph(self, name).map(|_| ())
    }
    fn drop_empty_schema(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_schema(name).map(|_| ())
    }
}
impl SchemaDropNotices for Engine {
    fn schema_drop_notice(&self, message: &str) {
        self.push_sql_notice("NOTICE", message);
    }
}
impl EmptySchemaRemovalState for Engine {
    fn schema_registry_write(&self) -> SchemaRegistryWrite<'_> {
        Box::new(self.durable.schemas.write())
    }
}
impl EmptySchemaRemovalPersistence for Engine {
    fn drop_schema_row(&self, name: &str) -> StorageBackendResult<()> {
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog.drop_schema(name)?;
        }
        Ok(())
    }
}

impl uqa_sql::catalog::resolution::candidates::RelationCandidateState for Engine {
    fn temporary_schema_name(&self) -> String {
        Engine::temporary_schema_name(self)
    }
    fn search_path(&self) -> uqa_sql::catalog::resolution::candidates::SearchPathRead<'_> {
        Box::new(parking_lot::RwLockReadGuard::map(
            self.session.state.read(),
            |state| &state.search_path,
        ))
    }
}
