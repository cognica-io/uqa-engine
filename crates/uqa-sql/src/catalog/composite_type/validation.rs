//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete composite declarations retain attribute-order precedence before live names and authority.

use super::StoredComposite;
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

pub fn validate_composite_registry(
    registry: &impl TypeDefinitionView<StoredComposite>,
    roles: &RoleDefinitions,
) -> Result<(), String> {
    validate_composite_registry_with_control(registry, roles, &ProductionControl::uncontrolled())
        .map_err(|error| match error {
            SQLError::Internal(message) => message,
            error => error.to_string(),
        })
}
pub fn validate_composite_registry_with_control(
    registry: &impl TypeDefinitionView<StoredComposite>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    inline(validate(registry.iter(), roles, control, &mut ()))
}
pub async fn validate_composite_registry_from_input(
    registry: &TypeDefinitions<StoredComposite>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    validate(registry.iter(), roles, control, work).await
}

async fn validate<'a>(
    entries: impl Iterator<Item = (&'a String, &'a StoredComposite)>,
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
                "invalid or duplicate composite identity for `{name}`"
            )));
        }
        for oid in [
            definition.oid,
            definition.array_oid,
            definition.relation_oid,
        ] {
            work.checkpoint().await?;
            if oid < crate::catalog::oids::FIRST_NORMAL_OBJECT_ID
                || oids.insert_copy(oid, ())?.is_some()
            {
                return Err(SQLError::Internal(format!(
                    "invalid or duplicate composite OID {oid} for `{name}`"
                )));
            }
        }
        if definition.identity.schema.is_empty()
            || definition.identity.name.is_empty()
            || *definition.identity.qualified_name_with_control(control)? != *name
            || !crate::catalog::array_type_names::valid_type_name(&definition.array_name)
        {
            return Err(SQLError::Internal(format!(
                "inconsistent composite name for `{name}`"
            )));
        }
        let invalid_attributes =
            || SQLError::Internal(format!("invalid composite attributes for `{name}`"));
        for pair in definition.attributes.windows(2) {
            work.checkpoint().await?;
            if pair[0].number >= pair[1].number {
                return Err(invalid_attributes());
            }
        }
        if definition
            .attributes
            .first()
            .is_some_and(|attribute| attribute.number < 1)
        {
            return Err(invalid_attributes());
        }
        let mut names = ProductionMap::new(*control);
        for attribute in &definition.attributes {
            work.checkpoint().await?;
            if !attribute.dropped && names.insert_copy(attribute.name.as_str(), ())?.is_some() {
                return Err(invalid_attributes());
            }
        }
        if find_identity(definition.owner, roles, control, work)
            .await?
            .is_none()
        {
            return Err(SQLError::Internal(format!(
                "composite `{name}` references missing role incarnation {}",
                definition.owner.oid
            )));
        }
    }
    control.check()?;
    Ok(())
}
