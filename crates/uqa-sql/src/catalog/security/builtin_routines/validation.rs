//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Builtin authority preserves tuple, grant and role-incarnation failure order.

use super::{object_acl, BuiltinRoutineSecurity, RoleDefinitions, RoleIdentity, SQLError};
use crate::catalog::{roles::identity::find_identity, work::Work};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{ProductionControl, ProductionMap},
};

pub(super) fn revision(entry: &BuiltinRoutineSecurity) -> Result<(), SQLError> {
    if entry.revision == [0; 16] {
        return Err(SQLError::Internal(
            "invalid builtin routine ACL tuple identity".into(),
        ));
    }
    Ok(())
}

pub(super) async fn validate_roles(
    identities: impl Iterator<Item = RoleIdentity>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    for identity in identities {
        if find_identity(identity, roles, control, work)
            .await?
            .is_none()
        {
            return Err(SQLError::Internal(
                "builtin routine ACL references missing role incarnation".into(),
            ));
        }
    }
    Ok(())
}

/// Resume the ordinary ordered ACL predicate and selected role lookup through the enclosing input. Every scratch node is admitted before allocation; no names or ACL entries are copied.
pub async fn validate_from_input(
    entry: &BuiltinRoutineSecurity,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    revision(entry)?;
    object_acl::validate_acl_from(
        RoleIdentity::BOOTSTRAP,
        Some(&entry.execute_acl),
        "builtin routine",
        control,
        input,
    )
    .await?;
    let mut identities = ProductionMap::new(*control);
    object_acl::collect_roles_from(
        RoleIdentity::BOOTSTRAP,
        Some(&entry.execute_acl),
        &mut identities,
        control,
        input,
    )
    .await?;
    validate_roles(
        identities.iter().map(|(role, ())| *role),
        roles,
        control,
        input,
    )
    .await?;
    control.check()?;
    Ok(())
}
