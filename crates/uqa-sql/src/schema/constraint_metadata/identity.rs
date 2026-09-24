//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocate constraint incarnations and preserve legacy NOT NULL OIDs during initial migration.

use super::{CatalogIdentityAllocator, ConstraintMetadataError, ConstraintMetadataResult};
use crate::ast::{ColumnDef, ConstraintCatalogIdentity};

pub mod claims;
pub mod foreign_keys;
pub mod keys;
pub mod legacy;

pub(super) fn materialize_key_identity(
    key: &mut crate::ast::TableKeyConstraint,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    if let Some(identity) = key.catalog_identity {
        if identity.is_valid() {
            return Ok(false);
        }
        return Err(ConstraintMetadataError::Invalid(
            "invalid key constraint catalog identity".into(),
        ));
    }
    let object_id = allocate.allocate_object_id("key constraint")?;
    key.catalog_identity = Some(ConstraintCatalogIdentity {
        object_id,
        oid: allocate.allocate_catalog_oid(super::CatalogOidClass::Constraint, &object_id)?,
    });
    if !key
        .catalog_identity
        .is_some_and(ConstraintCatalogIdentity::is_valid)
    {
        return Err(ConstraintMetadataError::Invalid(
            "invalid key constraint catalog identity".into(),
        ));
    }
    Ok(true)
}

pub(super) fn materialize_check_oid(
    object_id: Option<[u8; 16]>,
    oid: &mut Option<i64>,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    let object_id = object_id.ok_or_else(|| {
        ConstraintMetadataError::Invalid("CHECK has no catalog incarnation".into())
    })?;
    let changed = oid.is_none();
    let value = match *oid {
        Some(value) => value,
        None => allocate.allocate_catalog_oid(super::CatalogOidClass::Constraint, &object_id)?,
    };
    if !(ConstraintCatalogIdentity {
        object_id,
        oid: value,
    })
    .is_valid()
    {
        return Err(ConstraintMetadataError::Invalid(
            "invalid CHECK catalog identity".into(),
        ));
    }
    *oid = Some(value);
    Ok(changed)
}

pub(super) fn materialize_not_null_identity(
    column: &mut ColumnDef,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    if let Some(identity) = column.not_null_identity {
        validate(identity)?;
        return Ok(false);
    }
    let object_id = allocate.allocate_object_id("NOT NULL constraint")?;
    let identity = ConstraintCatalogIdentity {
        object_id,
        oid: allocate.allocate_catalog_oid(super::CatalogOidClass::Constraint, &object_id)?,
    };
    validate(identity)?;
    column.not_null_identity = Some(identity);
    Ok(true)
}

fn validate(identity: ConstraintCatalogIdentity) -> ConstraintMetadataResult<()> {
    if identity.is_valid() {
        Ok(())
    } else {
        Err(ConstraintMetadataError::Invalid(
            "invalid NOT NULL constraint catalog identity".into(),
        ))
    }
}

pub fn validate_not_null_identities(columns: &[ColumnDef]) -> ConstraintMetadataResult<()> {
    let mut identities = std::collections::BTreeSet::new();
    let mut oids = std::collections::BTreeSet::new();
    for column in columns {
        match (column.not_null, column.not_null_identity) {
            (true, Some(identity)) => {
                validate(identity)?;
                if !identities.insert(identity.object_id) || !oids.insert(identity.oid) {
                    return Err(ConstraintMetadataError::Invalid(
                        "duplicate NOT NULL constraint catalog identity".into(),
                    ));
                }
            }
            (true, None) => {
                return Err(ConstraintMetadataError::Invalid(
                    "NOT NULL constraint requires an initial catalog identity migration".into(),
                ))
            }
            (false, Some(_)) => {
                return Err(ConstraintMetadataError::Invalid(
                    "nullable column retains a NOT NULL constraint identity".into(),
                ))
            }
            (false, None) => {}
        }
    }
    Ok(())
}

/// Called only during the initial catalog transaction, before a legacy database becomes visible.
pub fn migrate_constraint_metadata(
    relation: &uqa_core::RelationIdentity,
    columns: &mut [ColumnDef],
    constraints: &mut crate::ast::TableConstraintSet,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    let missing: Vec<_> = columns
        .iter()
        .map(|column| column.not_null && column.not_null_identity.is_none())
        .collect();
    let changed = super::materialize_constraint_metadata(relation, columns, constraints, allocate)?;
    for (column, missing) in columns.iter_mut().zip(missing) {
        if missing {
            let name = column.not_null_name.as_deref().ok_or_else(|| {
                ConstraintMetadataError::Invalid("migrated NOT NULL constraint has no name".into())
            })?;
            let identity = column.not_null_identity.as_mut().ok_or_else(|| {
                ConstraintMetadataError::Invalid(
                    "migrated NOT NULL constraint has no identity".into(),
                )
            })?;
            identity.oid = crate::catalog::oids::stable_oid(
                "constraint",
                &format!("{}.{}.{name}", relation.schema, relation.name),
            );
        }
    }
    validate_not_null_identities(columns)?;
    Ok(changed)
}

#[cfg(test)]
mod tests;
