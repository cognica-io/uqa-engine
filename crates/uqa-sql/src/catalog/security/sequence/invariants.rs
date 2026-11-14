//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate sequence ACL endpoints and grant paths before catalog publication or migration.

use super::{AclPrivilege, SequenceSecurity};
use crate::catalog::roles::RoleDefinitions;
use crate::{
    catalog::{
        security::grant_paths,
        work::{inline, Work},
    },
    SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

pub fn validate_sequence_security_invariants(
    security: &SequenceSecurity,
    roles: &RoleDefinitions,
) -> Result<(), String> {
    inline(validate_from(
        security,
        roles,
        &ProductionControl::uncontrolled(),
        &mut (),
    ))
    .map_err(|error| match error {
        SQLError::Internal(message) => message,
        error => error.to_string(),
    })
}

pub(in crate::catalog::security) async fn validate_from(
    security: &SequenceSecurity,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    if !roles.contains_key(&security.role_owner) {
        return Err(SQLError::Internal(format!(
            "sequence references missing owner role `{}`",
            security.role_owner
        )));
    }
    let mut paths = ProductionMap::new(*control);
    for entry in security.acl.iter().flatten() {
        work.checkpoint().await?;
        control.check()?;
        let grantor = super::acl_grantor(entry, &security.role_owner);
        if entry
            .role
            .role_name()
            .is_some_and(|name| !roles.contains_key(name))
        {
            return Err(SQLError::Internal(format!(
                "ACL references missing grantee role `{}`",
                entry.role
            )));
        }
        if !roles.contains_key(grantor) {
            return Err(SQLError::Internal(format!(
                "ACL references missing grantor role `{grantor}`"
            )));
        }
        if paths.insert_copy((&entry.role, grantor), ())?.is_some() {
            return Err(SQLError::Internal(format!(
                "ACL contains duplicate grant path `{grantor}` -> `{}`",
                entry.role
            )));
        }
        if entry.privileges.is_empty() && entry.grant_options.is_empty() {
            return Err(SQLError::Internal(
                "ACL contains an empty grant path".into(),
            ));
        }
        if entry.role.is_public() && !entry.grant_options.is_empty() {
            return Err(SQLError::Internal(
                "PUBLIC cannot hold grant options".into(),
            ));
        }
        for privilege in [
            AclPrivilege::Select,
            AclPrivilege::Update,
            AclPrivilege::Usage,
        ] {
            let mask = privilege.mask();
            if entry.grant_options.intersects(mask) && !entry.privileges.intersects(mask) {
                return Err(SQLError::Internal(
                    "ACL grant option exists without its privilege".into(),
                ));
            }
            if (entry.privileges.intersects(mask) || entry.grant_options.intersects(mask))
                && grant_options(security, privilege, control, work)
                    .await?
                    .get(grantor)
                    .is_none()
            {
                return Err(SQLError::Internal(format!(
                    "ACL grant path from `{grantor}` is not rooted at owner `{}`",
                    security.role_owner
                )));
            }
        }
    }
    Ok(())
}

async fn grant_options<'a, 'c>(
    security: &'a SequenceSecurity,
    privilege: AclPrivilege,
    control: &ProductionControl<'c>,
    work: &mut impl Work,
) -> Result<ProductionMap<'c, &'a str, ()>, SQLError> {
    let mut reachable = ProductionMap::new(*control);
    reachable.insert_copy(security.role_owner.as_str(), ())?;
    grant_paths::extend_from(
        &security.role_owner,
        security.acl.as_deref(),
        privilege,
        &mut reachable,
        control,
        work,
    )
    .await?;
    Ok(reachable)
}
