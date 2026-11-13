//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine authority retains grant-path precedence and ordered role-incarnation checks.

use super::{bound_routine_owner, invalid, object_acl, CreateFunction, RoleDefinitions, SQLError};
use crate::catalog::{roles::identity::find_identity, work::Work};
use uqa_core::{
    catalog_role::RoleIdentity,
    image_io::AsyncImageInput,
    memory::{ProductionControl, ProductionMap},
};

/// Validate the original owner, grant paths and referenced role incarnations without producing a discarded collection of role names.
pub fn validate_routine_authority(
    definition: &CreateFunction,
    roles: &RoleDefinitions,
) -> Result<(), SQLError> {
    super::validate_routine_authority_identities(definition)?;
    crate::catalog::work::inline(validate_roles(
        super::authority_roles(definition)?.into_iter(),
        roles,
        &ProductionControl::uncontrolled(),
        &mut (),
    ))
}

/// Borrow the selected definition and role generation through the enclosing input's work windows. Grant closure and path validation finish before ordered role lookup; all temporary identity nodes retain the original allowance.
pub async fn validate_routine_authority_from_input(
    definition: &CreateFunction,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    validate_from(definition, roles, control, input).await
}

async fn validate_from(
    definition: &CreateFunction,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    let owner = bound_routine_owner(definition)?;
    let acl = definition.execute_acl.as_deref();
    object_acl::validate_acl_from(owner, acl, "routine", control, work).await?;
    let mut identities = ProductionMap::new(*control);
    object_acl::collect_roles_from(owner, acl, &mut identities, control, work).await?;
    validate_roles(
        identities.iter().map(|(identity, ())| *identity),
        roles,
        control,
        work,
    )
    .await
}

async fn validate_roles(
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
            return Err(invalid("missing role incarnation"));
        }
    }
    control.check()?;
    Ok(())
}
