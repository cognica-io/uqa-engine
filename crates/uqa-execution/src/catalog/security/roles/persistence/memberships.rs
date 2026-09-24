//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership records retain role incarnations and publish independent OID claims.

use super::{
    restoration, BTreeMap, CatalogFacade, RoleDefinition, RoleMembership, RoleMembershipKey,
    SQLError, StorageBackendError, StorageBackendResult, ROLE_MEMBERSHIPS_METADATA_KEY,
};
use serde::Deserialize;
use std::{collections::BTreeSet, fmt::Write};

pub(super) const PREFIX: &str = "uqa.sql.role_membership.v1:";
const OID_PREFIX: &str = "uqa.sql.role_membership_oid.v1:";
const FORMAT: &str = r#"{"role_membership_catalog_format":1}"#;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MembershipFormat {
    Aggregate,
    Records,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFormat {
    role_membership_catalog_format: u32,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredMemberships {
    Records(RecordFormat),
    Legacy(Vec<restoration::NamedRoleMembership>),
}

pub(super) fn key(membership: &RoleMembershipKey) -> String {
    let mut key = String::from(PREFIX);
    for identity in [membership.role, membership.member, membership.grantor] {
        write!(&mut key, "{:08x}", identity.oid).expect("string write");
        for byte in identity.object_id {
            write!(&mut key, "{byte:02x}").expect("string write");
        }
        key.push(':');
    }
    key
}

fn oid_key(oid: i64) -> String {
    format!("{OID_PREFIX}{oid}")
}

pub(super) fn read(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<(
    BTreeMap<RoleMembershipKey, RoleMembership>,
    MembershipFormat,
)> {
    let stored = catalog
        .get_metadata(ROLE_MEMBERSHIPS_METADATA_KEY)?
        .map(|json| serde_json::from_str::<StoredMemberships>(&json))
        .transpose()?;
    match stored {
        Some(StoredMemberships::Records(format)) => {
            if format.role_membership_catalog_format != 1 {
                return Err(StorageBackendError::Other(format!(
                    "unsupported role membership catalog format {}",
                    format.role_membership_catalog_format
                )));
            }
            let mut values = Vec::new();
            for (stored_key, json) in catalog.metadata_with_prefix(PREFIX)? {
                let membership: RoleMembership = serde_json::from_str(&json)?;
                if stored_key != key(&membership.key()) {
                    return Err(StorageBackendError::Other(
                        "role membership record key does not match its role identities".into(),
                    ));
                }
                values.push(membership);
            }
            let memberships = restoration::restore_role_memberships(roles, values)
                .map_err(StorageBackendError::Other)?;
            let owners = memberships
                .iter()
                .map(|(identity, membership)| (oid_key(membership.oid), key(identity)))
                .collect::<BTreeMap<_, _>>();
            let claims = catalog.metadata_with_prefix(OID_PREFIX)?;
            if claims.len() != owners.len()
                || claims
                    .iter()
                    .any(|(oid, owner)| owners.get(oid) != Some(owner))
            {
                return Err(StorageBackendError::Other(
                    "role membership OID records do not match membership identities".into(),
                ));
            }
            Ok((memberships, MembershipFormat::Records))
        }
        legacy => {
            if !catalog.metadata_with_prefix(PREFIX)?.is_empty()
                || !catalog.metadata_with_prefix(OID_PREFIX)?.is_empty()
            {
                return Err(StorageBackendError::Other(
                    "role membership records exist without their format marker".into(),
                ));
            }
            let memberships = match legacy {
                Some(StoredMemberships::Legacy(values)) => values,
                None => Vec::new(),
                _ => unreachable!(),
            };
            let memberships = restoration::restore_named_role_memberships(roles, memberships)
                .map_err(StorageBackendError::Other)?;
            Ok((memberships, MembershipFormat::Aggregate))
        }
    }
}

/// Initial restoration owns the transaction and validates both catalogs before publishing either conversion.
pub(super) fn migrate(
    catalog: &dyn CatalogFacade,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> StorageBackendResult<()> {
    for (identity, membership) in memberships {
        let key = key(identity);
        catalog.set_metadata(&key, &serde_json::to_string(membership)?)?;
        catalog.set_metadata(&oid_key(membership.oid), &key)?;
    }
    catalog.set_metadata(ROLE_MEMBERSHIPS_METADATA_KEY, FORMAT)
}

pub(super) fn persist(
    catalog: &dyn CatalogFacade,
    before: &BTreeMap<RoleMembershipKey, RoleMembership>,
    after: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Result<(), SQLError> {
    let mut oids = BTreeSet::new();
    for (identity, membership) in after {
        if identity != &membership.key()
            || membership.oid <= 0
            || membership.oid > i64::from(u32::MAX)
            || [identity.role, identity.member, identity.grantor]
                .iter()
                .any(|role| {
                    role.oid <= 0 || role.oid > i64::from(u32::MAX) || role.object_id == [0; 16]
                })
        {
            return Err(SQLError::Internal(
                "invalid role membership publication identity".into(),
            ));
        }
        if !oids.insert(membership.oid)
            || (before
                .get(identity)
                .is_none_or(|old| old.oid != membership.oid)
                && catalog
                    .get_metadata(&oid_key(membership.oid))
                    .map_err(persist_error)?
                    .is_some())
        {
            return Err(SQLError::Routine {
                sqlstate: "23505".into(),
                message: format!("role membership OID {} is already assigned", membership.oid),
            });
        }
    }
    for (identity, membership) in before {
        if after
            .get(identity)
            .is_none_or(|next| next.oid != membership.oid)
        {
            catalog
                .delete_metadata(&oid_key(membership.oid))
                .map_err(persist_error)?;
        }
        if !after.contains_key(identity) {
            catalog
                .delete_metadata(&key(identity))
                .map_err(persist_error)?;
        }
    }
    for (identity, membership) in after {
        if before.get(identity) == Some(membership) {
            continue;
        }
        let key = key(identity);
        if before
            .get(identity)
            .is_none_or(|old| old.oid != membership.oid)
        {
            catalog
                .set_metadata(&oid_key(membership.oid), &key)
                .map_err(persist_error)?;
        }
        let json = serde_json::to_string(membership).map_err(|error| {
            SQLError::Internal(format!("serialize role membership catalog: {error}"))
        })?;
        catalog.set_metadata(&key, &json).map_err(persist_error)?;
    }
    Ok(())
}

fn persist_error(error: StorageBackendError) -> SQLError {
    SQLError::Internal(format!("persist role membership catalog: {error}"))
}

#[cfg(test)]
mod tests;
