//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh namespace authority retains only schemas changed by the current transaction.

use std::sync::Arc;
use uqa_sql::catalog::roles::RoleDefinitions;
use uqa_sql::catalog::security::schema_registry::SchemaRegistry;
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &SchemaRegistry,
    committed: Arc<SchemaRegistry>,
    roles: &RoleDefinitions,
) -> StorageBackendResult<Arc<SchemaRegistry>> {
    merge_private_records(current, committed, roles, |name| {
        catalog.map_or(Ok(false), |catalog| {
            catalog.schema_has_private_changes(name)
        })
    })
}

fn merge_private_records(
    current: &SchemaRegistry,
    mut committed: Arc<SchemaRegistry>,
    roles: &RoleDefinitions,
    mut is_private: impl FnMut(&str) -> StorageBackendResult<bool>,
) -> StorageBackendResult<Arc<SchemaRegistry>> {
    {
        let names = current
            .keys()
            .chain(committed.keys())
            .collect::<std::collections::BTreeSet<_>>();
        let private = names
            .into_iter()
            .filter_map(|name| match is_private(name) {
                Ok(true) => Some(Ok(name.clone())),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        if !private.is_empty() {
            let merged = Arc::make_mut(&mut committed);
            for name in private {
                if let Some(security) = current.get(&name) {
                    merged.insert(name, security.clone()).map_err(|error| {
                        StorageBackendError::backend("schema definition", error)
                    })?;
                } else {
                    merged.remove(&name).map_err(|error| {
                        StorageBackendError::backend("schema definition", error)
                    })?;
                }
            }
        }
    }
    for security in committed.values() {
        security
            .validate(roles)
            .map_err(StorageBackendError::Other)?;
    }
    Ok(committed)
}

#[cfg(test)]
mod tests;
