//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Match attached key provenance to the local constraint incarnation through renames.

use super::validation;
use crate::ast::{TableConstraintSet, TableKeyConstraint};
use uqa_core::memory::{OwnedSet, ProductionControl};

pub fn provenance_matches(left: &TableKeyConstraint, right: &TableKeyConstraint) -> bool {
    match (left.catalog_identity, right.catalog_identity) {
        (Some(left), Some(right)) => left.object_id == right.object_id,
        _ => {
            crate::schema::inheritance::alter::key_equivalent(left, right)
                && (left.name.is_none() || right.name.is_none() || left.name == right.name)
        }
    }
}

pub fn synchronize_provenance(constraints: &mut TableConstraintSet) -> bool {
    let mut changed = false;
    for inherited in &mut constraints.hierarchy.partition_inherited_key_constraints {
        if let Some(local) = constraints
            .key_constraints
            .iter()
            .find(|key| provenance_matches(key, inherited))
        {
            if inherited != local {
                *inherited = local.clone();
                changed = true;
            }
        }
    }
    changed
}

pub fn validate_provenance(
    constraints: &TableConstraintSet,
) -> super::ConstraintMetadataResult<()> {
    validate_provenance_with_control(constraints, &ProductionControl::uncontrolled())
}

/// Retain inherited identity claims without copying key expressions or names.
pub fn validate_provenance_with_control(
    constraints: &TableConstraintSet,
    control: &ProductionControl<'_>,
) -> super::ConstraintMetadataResult<()> {
    crate::catalog::work::inline(validate_from(constraints, control, &mut ()))
}

pub(super) async fn validate_from(
    constraints: &TableConstraintSet,
    control: &ProductionControl<'_>,
    work: &mut impl crate::catalog::work::Work,
) -> super::ConstraintMetadataResult<()> {
    validation::check(control)?;
    let mut inherited_ids = OwnedSet::new();
    for inherited in &constraints.hierarchy.partition_inherited_key_constraints {
        validation::checkpoint(control, work).await?;
        let Some(identity) = inherited.catalog_identity else {
            return Err(super::ConstraintMetadataError::Invalid(
                "partition key provenance has no local catalog identity".into(),
            ));
        };
        if !validation::insert(&mut inherited_ids, identity.object_id, control)?
            || !has_local_key(constraints, inherited, control, work).await?
        {
            return Err(super::ConstraintMetadataError::Invalid(
                "partition key provenance does not identify one local constraint".into(),
            ));
        }
    }
    validation::check(control)
}

async fn has_local_key(
    constraints: &TableConstraintSet,
    inherited: &TableKeyConstraint,
    control: &ProductionControl<'_>,
    work: &mut impl crate::catalog::work::Work,
) -> super::ConstraintMetadataResult<bool> {
    for key in &constraints.key_constraints {
        validation::checkpoint(control, work).await?;
        if key.catalog_identity == inherited.catalog_identity
            && crate::schema::inheritance::alter::key_equivalent_from(key, inherited, control, work)
                .await
                .map_err(|error| super::ConstraintMetadataError::Execution(Box::new(error)))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}
