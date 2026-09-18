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

mod records;

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
    /// Preserve private role records, including deletions, while taking untouched roles from the latest committed catalog. Memberships still occupy one aggregate record.
    pub fn merge_private(
        mut self,
        catalog: Option<&dyn CatalogFacade>,
        current: &Self,
    ) -> StorageBackendResult<Self> {
        if let Some(catalog) = catalog {
            let names = self
                .roles
                .keys()
                .chain(current.roles.keys())
                .collect::<std::collections::BTreeSet<_>>();
            let mut roles = Arc::clone(&self.roles);
            for name in names {
                if catalog.metadata_has_private_changes(&records::role_key(name))? {
                    if let Some(role) = current.roles.get(name) {
                        Arc::make_mut(&mut roles).insert(name.clone(), role.clone());
                    } else {
                        Arc::make_mut(&mut roles).remove(name);
                    }
                }
            }
            self.roles = roles;
            if catalog.metadata_has_private_changes(ROLE_MEMBERSHIPS_METADATA_KEY)? {
                self.memberships = Arc::clone(&current.memberships);
            }
        }
        Ok(self)
    }
}

pub fn restore(catalog: &dyn CatalogFacade) -> StorageBackendResult<RoleCatalogValues> {
    restore_catalog(catalog, false)
}

pub fn restore_and_migrate(catalog: &dyn CatalogFacade) -> StorageBackendResult<RoleCatalogValues> {
    restore_catalog(catalog, true)
}

fn restore_catalog(
    catalog: &dyn CatalogFacade,
    allow_migration: bool,
) -> StorageBackendResult<RoleCatalogValues> {
    let (mut roles, legacy) = records::read(catalog)?;
    restoration::restore_role_definitions(&mut roles).map_err(StorageBackendError::Other)?;
    if !legacy {
        records::validate_oids(catalog, &roles)?;
    }
    let memberships = match catalog.get_metadata(ROLE_MEMBERSHIPS_METADATA_KEY)? {
        Some(json) => serde_json::from_str::<Vec<RoleMembership>>(&json)?,
        None => Vec::new(),
    };
    let memberships = restoration::restore_role_memberships(&roles, memberships)
        .map_err(StorageBackendError::Other)?;
    if legacy {
        if !allow_migration {
            return Err(StorageBackendError::Other(
                "role metadata requires initial-open record migration".into(),
            ));
        }
        records::migrate(catalog, &roles)?;
    }
    Ok(RoleCatalogValues { roles, memberships })
}

pub fn persist_roles(
    catalog: Option<&dyn CatalogFacade>,
    before: &BTreeMap<String, RoleDefinition>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    let Some(catalog) = catalog else {
        return Ok(());
    };
    records::persist(catalog, before, roles)
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
