//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable role records and their transaction-private catalog overlays.

use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{
    catalog::roles::{restoration, RoleDefinition, RoleMembership, RoleMembershipKey},
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub const ROLES_METADATA_KEY: &str = "sql_roles_json";
pub const ROLE_MEMBERSHIPS_METADATA_KEY: &str = "sql_role_memberships_json";

pub struct RoleCatalogValues {
    pub roles: BTreeMap<String, RoleDefinition>,
    pub memberships: BTreeMap<RoleMembershipKey, RoleMembership>,
}

pub struct RoleCatalogSnapshot {
    pub roles: Arc<BTreeMap<String, RoleDefinition>>,
    pub memberships: Arc<BTreeMap<RoleMembershipKey, RoleMembership>>,
}

impl RoleCatalogSnapshot {
    /// Each registry currently occupies one metadata record. Preserve exactly those records with private writes, including deletions, while taking other records from the latest committed catalog.
    pub fn merge_private(
        mut self,
        catalog: Option<&dyn CatalogFacade>,
        current: &Self,
    ) -> StorageBackendResult<Self> {
        if let Some(catalog) = catalog {
            if catalog.metadata_has_private_changes(ROLES_METADATA_KEY)? {
                self.roles = Arc::clone(&current.roles);
            }
            if catalog.metadata_has_private_changes(ROLE_MEMBERSHIPS_METADATA_KEY)? {
                self.memberships = Arc::clone(&current.memberships);
            }
        }
        Ok(self)
    }
}

pub fn restore(catalog: &dyn CatalogFacade) -> StorageBackendResult<RoleCatalogValues> {
    let mut roles = match catalog.get_metadata(ROLES_METADATA_KEY)? {
        Some(json) => serde_json::from_str::<BTreeMap<String, RoleDefinition>>(&json)?,
        None => BTreeMap::new(),
    };
    restoration::restore_role_definitions(&mut roles).map_err(StorageBackendError::Other)?;
    let memberships = match catalog.get_metadata(ROLE_MEMBERSHIPS_METADATA_KEY)? {
        Some(json) => serde_json::from_str::<Vec<RoleMembership>>(&json)?,
        None => Vec::new(),
    };
    let memberships = restoration::restore_role_memberships(&roles, memberships)
        .map_err(StorageBackendError::Other)?;
    Ok(RoleCatalogValues { roles, memberships })
}

pub fn persist_roles(
    catalog: Option<&dyn CatalogFacade>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    let Some(catalog) = catalog else {
        return Ok(());
    };
    let json = serde_json::to_string(roles)
        .map_err(|error| SQLError::Internal(format!("serialize role catalog: {error}")))?;
    catalog
        .set_metadata(ROLES_METADATA_KEY, &json)
        .map_err(|error| SQLError::Internal(format!("persist role catalog: {error}")))
}

pub fn persist_memberships(
    catalog: Option<&dyn CatalogFacade>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Result<(), SQLError> {
    let Some(catalog) = catalog else {
        return Ok(());
    };
    let stored = memberships.values().cloned().collect::<Vec<_>>();
    let json = serde_json::to_string(&stored).map_err(|error| {
        SQLError::Internal(format!("serialize role membership catalog: {error}"))
    })?;
    catalog
        .set_metadata(ROLE_MEMBERSHIPS_METADATA_KEY, &json)
        .map_err(|error| SQLError::Internal(format!("persist role membership catalog: {error}")))
}

#[cfg(test)]
mod tests;
