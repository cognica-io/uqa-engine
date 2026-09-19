//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh namespace authority retains only schemas changed by the current transaction.

use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::catalog::{roles::RoleDefinition, security::BoundSchemaSecurity};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &BTreeMap<String, BoundSchemaSecurity>,
    committed: Arc<BTreeMap<String, BoundSchemaSecurity>>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<Arc<BTreeMap<String, BoundSchemaSecurity>>> {
    merge_private_records(current, committed, roles, |name| {
        catalog.map_or(Ok(false), |catalog| {
            catalog.schema_has_private_changes(name)
        })
    })
}

fn merge_private_records(
    current: &BTreeMap<String, BoundSchemaSecurity>,
    mut committed: Arc<BTreeMap<String, BoundSchemaSecurity>>,
    roles: &BTreeMap<String, RoleDefinition>,
    mut is_private: impl FnMut(&str) -> StorageBackendResult<bool>,
) -> StorageBackendResult<Arc<BTreeMap<String, BoundSchemaSecurity>>> {
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
                    merged.insert(name, security.clone());
                } else {
                    merged.remove(&name);
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
