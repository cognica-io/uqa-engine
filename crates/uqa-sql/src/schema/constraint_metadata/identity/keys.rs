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
    validation::check(control)?;
    let mut inherited_ids = OwnedSet::new();
    for inherited in &constraints.hierarchy.partition_inherited_key_constraints {
        validation::check(control)?;
        let Some(identity) = inherited.catalog_identity else {
            return Err(super::ConstraintMetadataError::Invalid(
                "partition key provenance has no local catalog identity".into(),
            ));
        };
        if !validation::insert(&mut inherited_ids, identity.object_id, control)?
            || !validation::any(&constraints.key_constraints, control, |key| {
                key.catalog_identity == Some(identity)
                    && crate::schema::inheritance::alter::key_equivalent(key, inherited)
            })?
        {
            return Err(super::ConstraintMetadataError::Invalid(
                "partition key provenance does not identify one local constraint".into(),
            ));
        }
    }
    validation::check(control)
}
