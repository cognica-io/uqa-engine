//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum restoration and publication share the same ordered catalog predicates.

use super::{StoredEnum, MAX_ENUM_LABEL_BYTES};
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

/// Validate a complete enum registry before restoration exposes it or publication persists it.
pub fn validate_enum_registry(
    registry: &impl TypeDefinitionView<StoredEnum>,
    roles: &RoleDefinitions,
) -> Result<(), String> {
    validate_enum_registry_with_control(registry, roles, &ProductionControl::uncontrolled())
        .map_err(|error| match error {
            SQLError::Internal(message) => message,
            error => error.to_string(),
        })
}

pub fn validate_enum_registry_with_control(
    registry: &impl TypeDefinitionView<StoredEnum>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    inline(validate(registry.iter(), roles, control, &mut ()))
}

/// The enclosing input retains the complete immutable generation while each identity, label and role observation consumes its original work channel.
pub async fn validate_enum_registry_from_input(
    registry: &TypeDefinitions<StoredEnum>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    validate(registry.iter(), roles, control, input).await
}

async fn validate<'a>(
    entries: impl Iterator<Item = (&'a String, &'a StoredEnum)>,
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
                "invalid or duplicate enum identity for `{name}`"
            )));
        }
        for oid in [definition.oid, definition.array_oid]
            .into_iter()
            .chain(definition.label_oids())
        {
            work.checkpoint().await?;
            if oid < crate::catalog::oids::FIRST_NORMAL_OBJECT_ID
                || oids.insert_copy(oid, ())?.is_some()
            {
                return Err(SQLError::Internal(format!(
                    "invalid or duplicate enum OID {oid} for `{name}`"
                )));
            }
        }
        if definition.identity.schema.is_empty()
            || definition.identity.name.is_empty()
            || *definition.identity.qualified_name_with_control(control)? != *name
            || !crate::catalog::array_type_names::valid_type_name(&definition.array_name)
        {
            return Err(SQLError::Internal(format!(
                "inconsistent enum name for `{name}`"
            )));
        }
        let mut labels = ProductionMap::new(*control);
        for label in &definition.labels {
            work.checkpoint().await?;
            if label.label.len() > MAX_ENUM_LABEL_BYTES
                || labels.insert_copy(label.label.as_str(), ())?.is_some()
                || !label.sort_order.is_finite()
            {
                return Err(SQLError::Internal(format!(
                    "invalid enum label for `{name}`"
                )));
            }
        }
        for pair in definition.labels.windows(2) {
            work.checkpoint().await?;
            if pair[0].key >= pair[1].key || pair[0].sort_order >= pair[1].sort_order {
                return Err(SQLError::Internal(format!(
                    "enum labels for `{name}` are out of order"
                )));
            }
        }
        if find_identity(definition.owner, roles, control, work)
            .await?
            .is_none()
        {
            return Err(SQLError::Internal(format!(
                "enum `{name}` references missing role incarnation {}",
                definition.owner.oid
            )));
        }
    }
    control.check()?;
    Ok(())
}

#[cfg(test)]
mod tests;
