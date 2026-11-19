//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-key catalog rows have independent identities even when partition copies share enforcement.

use super::validation;
use crate::ast::{ColumnDef, ConstraintCatalogIdentity, TableConstraintSet};
use crate::schema::constraint_metadata::{
    CatalogIdentityAllocator, ConstraintMetadataError, ConstraintMetadataResult,
};
use uqa_core::memory::{OwnedSet, ProductionControl};
use uqa_core::RelationIdentity;

pub(crate) fn materialize(
    target: &mut Option<ConstraintCatalogIdentity>,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    if let Some(identity) = target {
        validate_identity(*identity)?;
        return Ok(false);
    }
    let object_id = allocate.allocate_object_id("foreign-key catalog row")?;
    let identity = ConstraintCatalogIdentity {
        object_id,
        oid: allocate.allocate_catalog_oid(
            crate::schema::constraint_metadata::CatalogOidClass::Constraint,
            &object_id,
        )?,
    };
    validate_identity(identity)?;
    *target = Some(identity);
    Ok(true)
}

fn validate_identity(identity: ConstraintCatalogIdentity) -> ConstraintMetadataResult<()> {
    if identity.is_valid() {
        Ok(())
    } else {
        Err(ConstraintMetadataError::Invalid(
            "invalid foreign-key catalog identity".into(),
        ))
    }
}

/// The catalog rows of a declaration's foreign keys, the constraints they derive on referenced partitions included.
pub fn identities<'a>(
    columns: &'a [ColumnDef],
    constraints: &'a TableConstraintSet,
) -> impl Iterator<Item = Option<ConstraintCatalogIdentity>> + 'a {
    identity_slots(columns, &constraints.foreign_keys).filter_map(|slot| match slot {
        IdentitySlot::Empty => None,
        IdentitySlot::Required(identity) => Some(identity),
    })
}

/// Empty declaration slots remain visible to bounded traversal even when they contribute no identity. An existing foreign key with no identity remains a distinct required claim.
pub enum IdentitySlot {
    Empty,
    Required(Option<ConstraintCatalogIdentity>),
}

/// Borrow every declaration and derived partition slot in catalog validation order.
pub fn identity_slots<'a>(
    columns: &'a [ColumnDef],
    keys: &'a [crate::ast::ForeignKey],
) -> impl Iterator<Item = IdentitySlot> + 'a {
    columns
        .iter()
        .map(|column| {
            column
                .references
                .as_ref()
                .map_or(IdentitySlot::Empty, |reference| {
                    IdentitySlot::Required(reference.catalog_identity)
                })
        })
        .chain(
            keys.iter()
                .map(|key| IdentitySlot::Required(key.catalog_identity)),
        )
        .chain(columns.iter().flat_map(|column| {
            std::iter::once(IdentitySlot::Empty).chain(column.references.iter().flat_map(
                |reference| {
                    reference
                        .referenced_partitions
                        .iter()
                        .map(|derived| IdentitySlot::Required(Some(derived.catalog_identity)))
                },
            ))
        }))
        .chain(keys.iter().flat_map(|key| {
            std::iter::once(IdentitySlot::Empty).chain(
                key.referenced_partitions
                    .iter()
                    .map(|derived| IdentitySlot::Required(Some(derived.catalog_identity))),
            )
        }))
}

pub fn validate(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
) -> ConstraintMetadataResult<()> {
    validate_with_control(columns, constraints, &ProductionControl::uncontrolled())
}

/// Borrow the original row order and partition provenance under the supplied temporary allowance.
pub fn validate_with_control(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
    control: &ProductionControl<'_>,
) -> ConstraintMetadataResult<()> {
    crate::catalog::work::inline(validate_from(columns, constraints, control, &mut ()))
}

pub async fn validate_from_input(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> ConstraintMetadataResult<()> {
    validate_from(columns, constraints, control, input).await
}

pub(super) async fn validate_from(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
    control: &ProductionControl<'_>,
    work: &mut impl crate::catalog::work::Work,
) -> ConstraintMetadataResult<()> {
    validation::check(control)?;
    let mut objects = OwnedSet::new();
    let mut oids = OwnedSet::new();
    for slot in identity_slots(columns, &constraints.foreign_keys) {
        validation::checkpoint(control, work).await?;
        let IdentitySlot::Required(identity) = slot else {
            continue;
        };
        let identity = identity.ok_or_else(|| {
            ConstraintMetadataError::Invalid(
                "foreign keys require an initial catalog identity migration".into(),
            )
        })?;
        validate_identity(identity)?;
        if !validation::insert(&mut objects, identity.object_id, control)?
            || !validation::insert(&mut oids, identity.oid, control)?
        {
            return Err(ConstraintMetadataError::Invalid(
                "duplicate foreign-key catalog identity".into(),
            ));
        }
    }
    for inherited in &constraints.hierarchy.partition_inherited_foreign_keys {
        validation::checkpoint(control, work).await?;
        if !validation::any(&constraints.foreign_keys, control, work, |key| {
            key.catalog_identity == inherited.catalog_identity
                && key.object_id == inherited.object_id
        })
        .await?
        {
            return Err(ConstraintMetadataError::Invalid(
                "partition foreign-key provenance does not identify its catalog row".into(),
            ));
        }
    }
    validation::check(control)
}

/// Capture missing identities before materialization so only legacy catalog rows retain name-derived OIDs.
pub struct LegacyIdentities {
    columns: Vec<usize>,
    constraints: Vec<usize>,
}

impl LegacyIdentities {
    pub fn capture(columns: &[ColumnDef], constraints: &TableConstraintSet) -> Self {
        Self {
            columns: columns
                .iter()
                .enumerate()
                .filter_map(|(index, column)| {
                    column
                        .references
                        .as_ref()
                        .filter(|key| key.catalog_identity.is_none())
                        .map(|_| index)
                })
                .collect(),
            constraints: constraints
                .foreign_keys
                .iter()
                .enumerate()
                .filter_map(|(index, key)| key.catalog_identity.is_none().then_some(index))
                .collect(),
        }
    }

    pub fn preserve_oids(
        self,
        relation: &RelationIdentity,
        columns: &mut [ColumnDef],
        constraints: &mut TableConstraintSet,
    ) -> ConstraintMetadataResult<bool> {
        let changed = !self.columns.is_empty() || !self.constraints.is_empty();
        for index in self.columns {
            let key = columns[index].references.as_mut().ok_or_else(|| {
                ConstraintMetadataError::Invalid("legacy foreign key disappeared".into())
            })?;
            preserve_oid(relation, key.name.as_deref(), key.catalog_identity.as_mut())?;
        }
        for index in self.constraints {
            let key = &mut constraints.foreign_keys[index];
            let previous = key.catalog_identity;
            preserve_oid(relation, key.name.as_deref(), key.catalog_identity.as_mut())?;
            let identity = key.catalog_identity;
            for inherited in &mut constraints.hierarchy.partition_inherited_foreign_keys {
                if inherited.catalog_identity == previous {
                    inherited.catalog_identity = identity;
                }
            }
        }
        super::super::synchronize_partition_inherited_foreign_key_ids(constraints);
        validate(columns, constraints)?;
        Ok(changed)
    }
}

fn preserve_oid(
    relation: &RelationIdentity,
    name: Option<&str>,
    identity: Option<&mut ConstraintCatalogIdentity>,
) -> ConstraintMetadataResult<()> {
    let name = name
        .ok_or_else(|| ConstraintMetadataError::Invalid("legacy foreign key has no name".into()))?;
    let identity = identity.ok_or_else(|| {
        ConstraintMetadataError::Invalid("legacy foreign key has no catalog identity".into())
    })?;
    identity.oid = crate::catalog::oids::stable_oid(
        "constraint",
        &format!("{}.{}.{name}", relation.schema, relation.name),
    );
    Ok(())
}

#[cfg(test)]
mod tests;
