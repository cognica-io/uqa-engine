//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable role records and their transaction-private catalog overlays.

use std::collections::BTreeMap;
use uqa_core::memory::RetainedValue;
use uqa_sql::catalog::roles::{RoleDefinitions, RoleMemberships};
use uqa_sql::{
    catalog::roles::{restoration, RoleDefinition, RoleMembership, RoleMembershipKey},
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

mod memberships;
mod records;

pub const ROLES_METADATA_KEY: &str = "sql_roles_json";
pub const ROLE_MEMBERSHIPS_METADATA_KEY: &str = "sql_role_memberships_json";

pub struct RoleCatalogValues {
    pub roles: RoleDefinitions,
    pub memberships: RoleMemberships,
}

#[derive(Clone)]
pub struct RoleCatalogSnapshot {
    pub roles: RetainedValue<RoleDefinitions>,
    pub memberships: RetainedValue<RoleMemberships>,
}

impl RoleCatalogSnapshot {
    /// Preserve private role and membership records, including deletions, while taking untouched records from the latest committed catalog.
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
            let mut roles = RetainedValue::clone(&self.roles);
            for name in names {
                if catalog.metadata_has_private_changes(&records::role_key(name))? {
                    if let Some(role) = current.roles.get(name) {
                        RetainedValue::make_mut(&mut roles)
                            .insert(name.clone(), role.clone())
                            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                    } else {
                        RetainedValue::make_mut(&mut roles)
                            .remove(name)
                            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                    }
                }
            }
            self.roles = roles;
            let identities = self
                .memberships
                .keys()
                .chain(current.memberships.keys())
                .collect::<std::collections::BTreeSet<_>>();
            let mut memberships = RetainedValue::clone(&self.memberships);
            for identity in identities {
                if catalog.metadata_has_private_changes(&memberships::key(identity))? {
                    if let Some(membership) = current.memberships.get(identity) {
                        RetainedValue::make_mut(&mut memberships)
                            .insert(*identity, membership.clone())
                            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                    } else {
                        RetainedValue::make_mut(&mut memberships)
                            .remove(identity)
                            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                    }
                }
            }
            self.memberships = memberships;
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
    let (mut roles, format) = records::read(catalog)?;
    restoration::restore_role_definitions(&mut roles).map_err(StorageBackendError::Other)?;
    if format != records::RoleRecordFormat::Aggregate {
        records::validate_oids(catalog, &roles)?;
    }
    if matches!(
        format,
        records::RoleRecordFormat::Aggregate | records::RoleRecordFormat::Definitions
    ) {
        // Complete a candidate catalog before binding legacy membership names. No metadata is written until both candidates have been validated.
        let mut restored = roles.clone();
        for (name, role) in &roles {
            if role.object_id == [0; 16] {
                let mut role = role.clone();
                role.object_id = if role.oid == 10 {
                    RoleDefinition::bootstrap().object_id
                } else {
                    crate::catalog::identity::new_nonzero_catalog_identity("role", "identity")?
                };
                restored
                    .insert(name.clone(), role)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            }
        }
        roles = restored;
    }
    restoration::validate_role_identities(&roles).map_err(StorageBackendError::Other)?;
    if format != records::RoleRecordFormat::Revisions {
        let mut restored = roles.clone();
        for (name, role) in &roles {
            let mut role = role.clone();
            role.revision = 1;
            restored
                .insert(name.clone(), role)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        roles = restored;
    }
    uqa_sql::catalog::roles::tuple::validate_revisions(&roles)
        .map_err(StorageBackendError::Other)?;
    let (memberships, membership_format) = memberships::read(catalog, &roles)?;
    if !allow_migration
        && (format != records::RoleRecordFormat::Revisions
            || membership_format != memberships::MembershipFormat::Records)
    {
        return Err(StorageBackendError::Other(
            "role metadata requires initial-open record migration".into(),
        ));
    }
    if format != records::RoleRecordFormat::Revisions {
        records::migrate(catalog, &roles)?;
    }
    if membership_format != memberships::MembershipFormat::Records {
        memberships::migrate(catalog, &memberships)?;
    }
    Ok(RoleCatalogValues { roles, memberships })
}

pub fn persist_roles(
    catalog: Option<&dyn CatalogFacade>,
    before: &RoleDefinitions,
    roles: &RoleDefinitions,
) -> Result<(), SQLError> {
    let Some(catalog) = catalog else {
        return Ok(());
    };
    records::persist(catalog, before, roles)
}

pub fn persist_memberships(
    catalog: Option<&dyn CatalogFacade>,
    before: &RoleMemberships,
    after: &RoleMemberships,
) -> Result<(), SQLError> {
    let Some(catalog) = catalog else {
        return Ok(());
    };
    memberships::persist(catalog, before, after)
}

#[cfg(test)]
mod tests;
