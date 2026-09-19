//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-key catalog rows have independent identities even when partition copies share enforcement.

use crate::ast::{ColumnDef, ConstraintCatalogIdentity, TableConstraintSet};
use crate::schema::constraint_metadata::{
    CatalogIdentityAllocator, ConstraintMetadataError, ConstraintMetadataResult,
};
use std::collections::BTreeSet;
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

pub fn identities<'a>(
    columns: &'a [ColumnDef],
    constraints: &'a TableConstraintSet,
) -> impl Iterator<Item = Option<ConstraintCatalogIdentity>> + 'a {
    columns
        .iter()
        .filter_map(|column| column.references.as_ref())
        .map(|reference| reference.catalog_identity)
        .chain(
            constraints
                .foreign_keys
                .iter()
                .map(|key| key.catalog_identity),
        )
}

pub fn validate(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
) -> ConstraintMetadataResult<()> {
    let mut objects = BTreeSet::new();
    let mut oids = BTreeSet::new();
    for identity in identities(columns, constraints) {
        let identity = identity.ok_or_else(|| {
            ConstraintMetadataError::Invalid(
                "foreign keys require an initial catalog identity migration".into(),
            )
        })?;
        validate_identity(identity)?;
        if !objects.insert(identity.object_id) || !oids.insert(identity.oid) {
            return Err(ConstraintMetadataError::Invalid(
                "duplicate foreign-key catalog identity".into(),
            ));
        }
    }
    for inherited in &constraints.hierarchy.partition_inherited_foreign_keys {
        if !constraints.foreign_keys.iter().any(|key| {
            key.catalog_identity == inherited.catalog_identity
                && key.object_id == inherited.object_id
        }) {
            return Err(ConstraintMetadataError::Invalid(
                "partition foreign-key provenance does not identify its catalog row".into(),
            ));
        }
    }
    Ok(())
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
            preserve_oid(relation, key.name.as_deref(), key.catalog_identity.as_mut())?;
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
