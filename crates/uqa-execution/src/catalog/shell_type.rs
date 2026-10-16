//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Undefined type authority uses the same independent records, OID claims and private-overlay rules as completed types.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
    sync::Arc,
};
use uqa_sql::catalog::{
    roles::RoleDefinition,
    shell_type::{validate_shell_registry, StoredShellType},
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

use super::type_records::{TypeRecord, TypeRecordFormat};

pub type ShellRegistry = BTreeMap<String, StoredShellType>;
pub type ShellRegistryRead<'a> = Box<dyn Deref<Target = ShellRegistry> + 'a>;

pub trait ShellRegistryPublication {
    fn shell_registry(&self) -> ShellRegistryRead<'_>;
    fn shell_catalog(&self) -> Option<&dyn CatalogFacade>;
    fn shell_role_definitions(&self) -> BTreeMap<String, RoleDefinition>;
    fn publish_shell_definitions(&self, registry: ShellRegistry);
}

const FORMAT: TypeRecordFormat = TypeRecordFormat {
    label: "shell",
    definition_context: "shell definition",
    oid_context: "shell OID",
    format_key: "sql_shell_types_json",
    format_field: "shell_catalog_format",
    version: 1,
    prefix: "uqa.sql.shell.v1:",
    oid_prefix: "uqa.sql.shell_oid.v1:",
};

impl TypeRecord for StoredShellType {
    fn claimed_oids(&self) -> BTreeSet<u32> {
        BTreeSet::from([self.oid])
    }
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<ShellRegistry> {
    let registry = FORMAT.read(catalog)?;
    validate_shell_registry(&registry, roles).map_err(StorageBackendError::Other)?;
    Ok(registry)
}

pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &Arc<ShellRegistry>,
    committed: Arc<ShellRegistry>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<Arc<ShellRegistry>> {
    let registry = FORMAT.merge_private(catalog, current, committed)?;
    validate_shell_registry(&registry, roles).map_err(StorageBackendError::Other)?;
    Ok(registry)
}

/// Persist names and their single OID claim in the caller's definition transaction, then expose the validated generation.
pub fn publish(
    publication: &dyn ShellRegistryPublication,
    before: &ShellRegistry,
    registry: ShellRegistry,
) -> Result<(), uqa_sql::SQLError> {
    let registry = {
        let current = publication.shell_registry();
        let registry = FORMAT
            .merge_changes(before, &registry, &current)
            .map_err(|error| {
                uqa_sql::catalog::errors::storage_error("prepare shell catalog", &error)
            })?;
        validate_shell_registry(&registry, &publication.shell_role_definitions())
            .map_err(uqa_sql::SQLError::Internal)?;
        if let Some(catalog) = publication.shell_catalog() {
            FORMAT
                .persist(catalog, &current, &registry)
                .map_err(|error| {
                    uqa_sql::catalog::errors::storage_error("persist shell catalog", &error)
                })?;
        }
        registry
    };
    publication.publish_shell_definitions(registry);
    Ok(())
}

#[cfg(test)]
mod tests;
