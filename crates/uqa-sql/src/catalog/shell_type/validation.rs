//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Undefined type validation retains original name and incarnation precedence.

use super::StoredShellType;
use crate::{
    catalog::{
        roles::{identity::find_identity, RoleDefinitions},
        type_definitions::{TypeDefinitionView, TypeDefinitions},
        work::{inline, Work},
    },
    SQLError,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{ProductionControl, ProductionMap},
};

/// Reject inconsistent names, duplicate identities and stale owner incarnations before publishing any restored shell.
pub fn validate_shell_registry(
    registry: &impl TypeDefinitionView<StoredShellType>,
    roles: &RoleDefinitions,
) -> Result<(), String> {
    validate_shell_registry_with_control(registry, roles, &ProductionControl::uncontrolled())
        .map_err(|error| match error {
            SQLError::Internal(message) => message,
            error => error.to_string(),
        })
}

pub fn validate_shell_registry_with_control(
    registry: &impl TypeDefinitionView<StoredShellType>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    inline(validate(registry.iter(), roles, control, &mut ()))
}

pub async fn validate_shell_registry_from_input(
    registry: &TypeDefinitions<StoredShellType>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    validate(registry.iter(), roles, control, input).await
}

async fn validate<'a>(
    entries: impl Iterator<Item = (&'a String, &'a StoredShellType)>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    let mut identities = ProductionMap::new(*control);
    let mut oids = ProductionMap::new(*control);
    for (name, definition) in entries {
        work.checkpoint().await?;
        control.check()?;
        if definition.object_id == [0; 16]
            || identities.insert_copy(definition.object_id, ())?.is_some()
        {
            return Err(SQLError::Internal(format!(
                "invalid or duplicate shell identity for `{name}`"
            )));
        }
        if definition.oid < crate::catalog::oids::FIRST_NORMAL_OBJECT_ID
            || oids.insert_copy(definition.oid, ())?.is_some()
        {
            return Err(SQLError::Internal(format!(
                "invalid or duplicate shell OID {} for `{name}`",
                definition.oid
            )));
        }
        if definition.identity.schema.is_empty()
            || !crate::catalog::array_type_names::valid_type_name(&definition.identity.name)
            || *definition.identity.qualified_name_with_control(control)? != *name
        {
            return Err(SQLError::Internal(format!(
                "inconsistent shell name for `{name}`"
            )));
        }
        if find_identity(definition.owner, roles, control, work)
            .await?
            .is_none()
        {
            return Err(SQLError::Internal(format!(
                "shell `{name}` references missing role incarnation {}",
                definition.owner.oid
            )));
        }
    }
    control.check()?;
    Ok(())
}

#[cfg(test)]
mod tests;
