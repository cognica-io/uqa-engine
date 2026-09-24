//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read catalog addresses without binding expressions, resolving references or projecting query rows.

use crate::ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint};
use uqa_core::RelationIdentity;

/// Existing rows in an incomplete declaration candidate. Missing identities are filled by materialization, while supplied identities are validated before allocation.
pub fn identities(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
) -> Vec<crate::ast::ConstraintCatalogIdentity> {
    row_identities(
        columns,
        &constraints.checks,
        &constraints.key_constraints,
        &constraints.foreign_keys,
    )
    .collect()
}

/// Inspect stored row identities without cloning expressions or attachment metadata.
pub fn row_identities<'a>(
    columns: &'a [ColumnDef],
    checks: &'a [TableCheck],
    keys: &'a [TableKeyConstraint],
    foreign_keys: &'a [ForeignKey],
) -> impl Iterator<Item = crate::ast::ConstraintCatalogIdentity> + 'a {
    columns
        .iter()
        .filter_map(|column| column.not_null_identity)
        .chain(
            columns
                .iter()
                .filter_map(|column| column.references.as_ref()?.catalog_identity),
        )
        .chain(foreign_keys.iter().filter_map(|key| key.catalog_identity))
        .chain(keys.iter().filter_map(|key| key.catalog_identity))
        .chain(columns.iter().filter_map(|column| {
            column
                .check_object_id
                .zip(column.check_catalog_oid)
                .map(|(object_id, oid)| crate::ast::ConstraintCatalogIdentity { object_id, oid })
        }))
        .chain(checks.iter().filter_map(|check| {
            check
                .object_id
                .zip(check.catalog_oid)
                .map(|(object_id, oid)| crate::ast::ConstraintCatalogIdentity { object_id, oid })
        }))
}

pub fn validate_present_identities(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
) -> super::ConstraintMetadataResult<()> {
    for column in columns {
        if column.check.is_none()
            && (column.check_object_id.is_some() || column.check_catalog_oid.is_some())
        {
            return Err(invalid(
                "column without CHECK retains a constraint identity",
            ));
        }
        if column.check_catalog_oid.is_some() && column.check_object_id.is_none() {
            return Err(invalid(
                "CHECK catalog address has no constraint incarnation",
            ));
        }
    }
    if constraints
        .checks
        .iter()
        .any(|check| check.catalog_oid.is_some() && check.object_id.is_none())
    {
        return Err(invalid(
            "CHECK catalog address has no constraint incarnation",
        ));
    }
    let mut objects = std::collections::BTreeSet::new();
    let mut oids = std::collections::BTreeSet::new();
    for identity in identities(columns, constraints) {
        let kind = if columns
            .iter()
            .any(|column| column.not_null_identity == Some(identity))
        {
            "NOT NULL constraint"
        } else if super::foreign_keys::identities(columns, constraints)
            .any(|foreign| foreign == Some(identity))
        {
            "foreign-key"
        } else {
            "key or CHECK constraint"
        };
        if !identity.is_valid() {
            return Err(invalid(&format!("invalid {kind} catalog identity")));
        }
        if !objects.insert(identity.object_id) || !oids.insert(identity.oid) {
            return Err(invalid(&format!("duplicate {kind} catalog identity")));
        }
    }
    Ok(())
}

/// Current persisted definitions must have complete identities; load-only paths never repair them.
pub fn validate_constraint_identities(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
) -> super::ConstraintMetadataResult<()> {
    super::validate_not_null_identities(columns)?;
    super::foreign_keys::validate(columns, constraints)?;
    validate_key_and_check_identities(columns, constraints)
}

pub fn validate_key_and_check_identities(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
) -> super::ConstraintMetadataResult<()> {
    validate_present_identities(columns, constraints)?;
    if constraints
        .key_constraints
        .iter()
        .any(|key| key.catalog_identity.is_none())
    {
        return Err(invalid(
            "key constraints require an initial catalog identity migration",
        ));
    }
    if columns.iter().any(|column| {
        column.check.is_some()
            && (column.check_object_id.is_none() || column.check_catalog_oid.is_none())
    }) || constraints
        .checks
        .iter()
        .any(|check| check.object_id.is_none() || check.catalog_oid.is_none())
    {
        return Err(invalid(
            "CHECK constraints require an initial catalog identity migration",
        ));
    }
    super::keys::validate_provenance(constraints)
}

fn invalid(message: &str) -> super::ConstraintMetadataError {
    super::ConstraintMetadataError::Invalid(message.into())
}

#[cfg(test)]
mod tests;

pub fn has_constraint_oid(
    relation: &RelationIdentity,
    columns: &[ColumnDef],
    checks: &[TableCheck],
    keys: &[TableKeyConstraint],
    foreign_keys: &[ForeignKey],
    oid: i64,
) -> bool {
    let matches = |name: Option<&str>, stored: Option<i64>, object: Option<[u8; 16]>| {
        stored
            .or_else(|| {
                object.map(|object| crate::catalog::oids::stable_object_oid("constraint", &object))
            })
            .or_else(|| {
                name.map(|name| {
                    crate::catalog::oids::stable_oid(
                        "constraint",
                        &format!("{}.{}.{name}", relation.schema, relation.name),
                    )
                })
            })
            == Some(oid)
    };
    columns.iter().any(|column| {
        (column.not_null
            && matches(
                column.not_null_name.as_deref(),
                column.not_null_identity.map(|identity| identity.oid),
                None,
            ))
            || (column.check.is_some()
                && matches(
                    column.check_name.as_deref(),
                    column.check_catalog_oid,
                    column.check_object_id,
                ))
            || column.references.as_ref().is_some_and(|reference| {
                matches(
                    reference.name.as_deref(),
                    reference.catalog_identity.map(|identity| identity.oid),
                    None,
                )
            })
    }) || checks
        .iter()
        .any(|check| matches(check.name.as_deref(), check.catalog_oid, check.object_id))
        || keys.iter().any(|key| {
            matches(
                key.name.as_deref(),
                key.catalog_identity.map(|identity| identity.oid),
                None,
            )
        })
        || foreign_keys.iter().any(|key| {
            matches(
                key.name.as_deref(),
                key.catalog_identity.map(|identity| identity.oid),
                None,
            )
        })
}
