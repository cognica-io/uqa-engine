//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Match attached key provenance to the local constraint incarnation through renames.

use crate::ast::{TableConstraintSet, TableKeyConstraint};

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
    let mut inherited_ids = std::collections::BTreeSet::new();
    for inherited in &constraints.hierarchy.partition_inherited_key_constraints {
        let Some(identity) = inherited.catalog_identity else {
            return Err(super::ConstraintMetadataError::Invalid(
                "partition key provenance has no local catalog identity".into(),
            ));
        };
        if !inherited_ids.insert(identity.object_id)
            || !constraints.key_constraints.iter().any(|key| {
                key.catalog_identity == Some(identity)
                    && crate::schema::inheritance::alter::key_equivalent(key, inherited)
            })
        {
            return Err(super::ConstraintMetadataError::Invalid(
                "partition key provenance does not identify one local constraint".into(),
            ));
        }
    }
    Ok(())
}
