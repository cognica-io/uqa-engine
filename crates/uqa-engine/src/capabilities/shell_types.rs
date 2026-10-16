//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain shell type generations and adapt their existing catalog transaction owner.

use super::super::Engine;
use uqa_execution::catalog::shell_type::{
    ShellRegistry, ShellRegistryPublication, ShellRegistryRead,
};

impl Engine {
    pub(crate) fn shell_completion_context(
        &self,
    ) -> uqa_execution::schema::shell_types::ShellCompletionContext<'_> {
        uqa_execution::schema::shell_types::ShellCompletionContext {
            publication: self,
            locks: self,
        }
    }
    pub(crate) fn shell_type_context(
        &self,
    ) -> uqa_execution::schema::shell_types::ShellTypeContext<'_> {
        uqa_execution::schema::shell_types::ShellTypeContext {
            registries: uqa_execution::schema::types::arrays::UserTypeRegistries {
                enums: self,
                domains: self,
                composites: self,
            },
            creation: self.relation_creation_context(),
            identities: self.catalog_identity_reservation_context(),
            writer: self,
            allocate_identity: || {
                crate::new_nonzero_catalog_identity("shell", "object identity")
                    .map_err(|error| uqa_sql::SQLError::Internal(error.to_string()))
            },
            publication: self,
            changes: self,
        }
    }

    pub(crate) fn restore_shells_from_catalog(
        &self,
        catalog: &dyn uqa_storage::CatalogFacade,
    ) -> uqa_storage::StorageBackendResult<()> {
        *self.durable.shells.write() =
            uqa_execution::catalog::shell_type::restore(catalog, &self.durable.roles.read())?;
        Ok(())
    }
}

impl ShellRegistryPublication for Engine {
    fn shell_registry(&self) -> ShellRegistryRead<'_> {
        Box::new(self.durable.shells.read())
    }
    fn shell_catalog(&self) -> Option<&dyn uqa_storage::CatalogFacade> {
        self.storage.catalog.as_deref()
    }
    fn shell_role_definitions(
        &self,
    ) -> std::collections::BTreeMap<String, uqa_sql::catalog::roles::RoleDefinition> {
        self.durable.roles.read().clone()
    }
    fn publish_shell_definitions(&self, registry: ShellRegistry) {
        *self.durable.shells.write() = registry;
    }
}
