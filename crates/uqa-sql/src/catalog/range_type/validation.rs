//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range identities, names, authority and support observations share their original validation order.

use super::StoredRange;
use crate::{
    ast::ColumnType,
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

pub fn validate_range_registry(
    registry: &impl TypeDefinitionView<StoredRange>,
    roles: &RoleDefinitions,
) -> Result<(), String> {
    validate_range_registry_with_control(registry, roles, &ProductionControl::uncontrolled())
        .map_err(|error| match error {
            SQLError::Internal(message) => message,
            error => error.to_string(),
        })
}
pub fn validate_range_registry_with_control(
    registry: &impl TypeDefinitionView<StoredRange>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    inline(validate(registry.iter(), roles, control, &mut ()))
}
pub async fn validate_range_registry_from_input(
    registry: &TypeDefinitions<StoredRange>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    validate(registry.iter(), roles, control, work).await
}

async fn validate<'a>(
    entries: impl Iterator<Item = (&'a String, &'a StoredRange)>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    let mut identities = ProductionMap::new(*control);
    let mut oids = ProductionMap::new(*control);
    let mut names = ProductionMap::new(*control);
    for (name, definition) in entries {
        work.checkpoint().await?;
        control.check()?;
        for identity in [definition.object_id, definition.multirange.object_id] {
            work.checkpoint().await?;
            if identity == [0; 16] || identities.insert_copy(identity, ())?.is_some() {
                return Err(SQLError::Internal(format!(
                    "invalid or duplicate range identity for `{name}`"
                )));
            }
        }
        for oid in definition.type_oids() {
            work.checkpoint().await?;
            if oid < crate::catalog::oids::FIRST_NORMAL_OBJECT_ID
                || oids.insert_copy(oid, ())?.is_some()
            {
                return Err(SQLError::Internal(format!(
                    "invalid or duplicate range OID {oid} for `{name}`"
                )));
            }
        }
        if *definition.identity.qualified_name_with_control(control)? != *name {
            return Err(SQLError::Internal(format!(
                "inconsistent range name for `{name}`"
            )));
        }
        for (identity, array_name) in [
            (&definition.identity, &definition.array_name),
            (
                &definition.multirange.identity,
                &definition.multirange.array_name,
            ),
        ] {
            work.checkpoint().await?;
            if identity.schema.is_empty()
                || !crate::catalog::array_type_names::valid_type_name(&identity.name)
                || !crate::catalog::array_type_names::valid_type_name(array_name)
                || names
                    .insert_copy((identity.schema.as_str(), identity.name.as_str()), ())?
                    .is_some()
                || names
                    .insert_copy((identity.schema.as_str(), array_name.as_str()), ())?
                    .is_some()
            {
                return Err(SQLError::Internal(format!(
                    "invalid or duplicate range type name for `{name}`"
                )));
            }
        }
        if find_identity(definition.owner, roles, control, work)
            .await?
            .is_none()
        {
            return Err(SQLError::Internal(format!(
                "range `{name}` references missing role incarnation {}",
                definition.owner.oid
            )));
        }
        if !subtype(&definition.subtype, control, work).await?
            || definition.subtype_operator_class == 0
            || definition.canonical_oid == Some(0)
            || definition.subtype_difference_oid == Some(0)
        {
            return Err(SQLError::Internal(format!(
                "invalid range support identity for `{name}`"
            )));
        }
    }
    control.check()?;
    Ok(())
}

pub(crate) fn resolved_subtype(ty: &ColumnType) -> bool {
    inline(subtype(ty, &ProductionControl::uncontrolled(), &mut ()))
        .expect("inline subtype traversal has no fallible work")
}

async fn subtype(
    mut ty: &ColumnType,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<bool, SQLError> {
    loop {
        work.checkpoint().await?;
        control.check()?;
        match ty {
            ColumnType::Named(_) => return Ok(false),
            ColumnType::Array(element) => {
                let mut element = element.as_ref();
                while let ColumnType::Array(inner) = element {
                    work.checkpoint().await?;
                    control.check()?;
                    element = inner;
                }
                if matches!(element, ColumnType::Named(_))
                    || crate::catalog::type_metadata::pg_type_array_oid(element) == 0
                {
                    return Ok(false);
                }
                ty = element;
            }
            ColumnType::Domain { oid, base, .. } => {
                if *oid == 0 {
                    return Ok(false);
                }
                ty = base;
            }
            _ => return Ok(crate::catalog::type_metadata::pg_type_oid(ty) != 0),
        }
    }
}
