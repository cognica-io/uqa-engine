//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read catalog addresses without binding expressions, resolving references or projecting query rows.

use super::validation;
use crate::ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint};
use uqa_core::{
    memory::{OwnedSet, ProductionControl},
    RelationIdentity,
};

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
    row_slots(columns, checks, keys, foreign_keys).flatten()
}

fn row_slots<'a>(
    columns: &'a [ColumnDef],
    checks: &'a [TableCheck],
    keys: &'a [TableKeyConstraint],
    foreign_keys: &'a [ForeignKey],
) -> impl Iterator<Item = Option<crate::ast::ConstraintCatalogIdentity>> + 'a {
    columns
        .iter()
        .map(|column| column.not_null_identity)
        .chain(
            super::foreign_keys::identity_slots(columns, foreign_keys).map(|slot| match slot {
                super::foreign_keys::IdentitySlot::Empty => None,
                super::foreign_keys::IdentitySlot::Required(identity) => identity,
            }),
        )
        .chain(keys.iter().map(|key| key.catalog_identity))
        .chain(columns.iter().map(|column| {
            column
                .check_object_id
                .zip(column.check_catalog_oid)
                .map(|(object_id, oid)| crate::ast::ConstraintCatalogIdentity { object_id, oid })
        }))
        .chain(checks.iter().map(|check| {
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
    validate_present_identities_with_control(
        columns,
        constraints,
        &ProductionControl::uncontrolled(),
    )
}

/// Inspect present catalog identities without materializing an intermediate row vector.
pub fn validate_present_identities_with_control(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    control: &ProductionControl<'_>,
) -> super::ConstraintMetadataResult<()> {
    crate::catalog::work::inline(validate_present_from(
        columns,
        constraints,
        control,
        &mut (),
    ))
}

async fn validate_present_from(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    control: &ProductionControl<'_>,
    work: &mut impl crate::catalog::work::Work,
) -> super::ConstraintMetadataResult<()> {
    validation::check(control)?;
    for column in columns {
        validation::checkpoint(control, work).await?;
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
    if validation::any(&constraints.checks, control, work, |check| {
        check.catalog_oid.is_some() && check.object_id.is_none()
    })
    .await?
    {
        return Err(invalid(
            "CHECK catalog address has no constraint incarnation",
        ));
    }
    let mut objects = OwnedSet::new();
    let mut oids = OwnedSet::new();
    for identity in row_slots(
        columns,
        &constraints.checks,
        &constraints.key_constraints,
        &constraints.foreign_keys,
    ) {
        validation::checkpoint(control, work).await?;
        let Some(identity) = identity else { continue };
        let kind = if validation::any(columns, control, work, |column| {
            column.not_null_identity == Some(identity)
        })
        .await?
        {
            "NOT NULL constraint"
        } else if validation::any(
            super::foreign_keys::identity_slots(columns, &constraints.foreign_keys),
            control,
            work,
            |slot| matches!(slot, super::foreign_keys::IdentitySlot::Required(Some(foreign)) if foreign == identity),
        )
        .await?
        {
            "foreign-key"
        } else {
            "key or CHECK constraint"
        };
        if !identity.is_valid() {
            return Err(invalid(&format!("invalid {kind} catalog identity")));
        }
        if !validation::insert(&mut objects, identity.object_id, control)?
            || !validation::insert(&mut oids, identity.oid, control)?
        {
            return Err(invalid(&format!("duplicate {kind} catalog identity")));
        }
    }
    validation::check(control)
}

/// Current persisted definitions must have complete identities; load-only paths never repair them.
pub fn validate_constraint_identities(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
) -> super::ConstraintMetadataResult<()> {
    validate_constraint_identities_with_control(
        columns,
        constraints,
        &ProductionControl::uncontrolled(),
    )
}

/// Validate complete persisted row identities under the original allowance without migrations or syntax reconstruction.
pub fn validate_constraint_identities_with_control(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    control: &ProductionControl<'_>,
) -> super::ConstraintMetadataResult<()> {
    crate::catalog::work::inline(validate_constraints_from(
        columns,
        constraints,
        control,
        &mut (),
    ))
}

pub async fn validate_constraint_identities_from_input(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> super::ConstraintMetadataResult<()> {
    validate_constraints_from(columns, constraints, control, input).await
}

async fn validate_constraints_from(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    control: &ProductionControl<'_>,
    work: &mut impl crate::catalog::work::Work,
) -> super::ConstraintMetadataResult<()> {
    super::validate_not_null_from(columns, control, work).await?;
    super::foreign_keys::validate_from(columns, constraints, control, work).await?;
    validate_keys_from(columns, constraints, control, work).await
}

pub fn validate_key_and_check_identities(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
) -> super::ConstraintMetadataResult<()> {
    validate_key_and_check_identities_with_control(
        columns,
        constraints,
        &ProductionControl::uncontrolled(),
    )
}

pub fn validate_key_and_check_identities_with_control(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    control: &ProductionControl<'_>,
) -> super::ConstraintMetadataResult<()> {
    crate::catalog::work::inline(validate_keys_from(columns, constraints, control, &mut ()))
}

pub async fn validate_key_and_check_identities_from_input(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> super::ConstraintMetadataResult<()> {
    validate_keys_from(columns, constraints, control, input).await
}

async fn validate_keys_from(
    columns: &[ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
    control: &ProductionControl<'_>,
    work: &mut impl crate::catalog::work::Work,
) -> super::ConstraintMetadataResult<()> {
    validate_present_from(columns, constraints, control, work).await?;
    if validation::any(&constraints.key_constraints, control, work, |key| {
        key.catalog_identity.is_none()
    })
    .await?
    {
        return Err(invalid(
            "key constraints require an initial catalog identity migration",
        ));
    }
    if validation::any(columns, control, work, |column| {
        column.check.is_some()
            && (column.check_object_id.is_none() || column.check_catalog_oid.is_none())
    })
    .await?
        || validation::any(&constraints.checks, control, work, |check| {
            check.object_id.is_none() || check.catalog_oid.is_none()
        })
        .await?
    {
        return Err(invalid(
            "CHECK constraints require an initial catalog identity migration",
        ));
    }
    super::keys::validate_from(constraints, control, work).await
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
    constraint_oids(relation, columns, checks, keys, foreign_keys).any(|value| value == oid)
}

/// Borrow the same recorded and legacy addresses used by catalog membership checks.
pub fn constraint_oids<'a>(
    relation: &'a RelationIdentity,
    columns: &'a [ColumnDef],
    checks: &'a [TableCheck],
    keys: &'a [TableKeyConstraint],
    foreign_keys: &'a [ForeignKey],
) -> impl Iterator<Item = i64> + 'a {
    let address = move |name: Option<&str>, stored: Option<i64>, object: Option<[u8; 16]>| {
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
    };
    columns
        .iter()
        .flat_map(move |column| {
            [
                column
                    .not_null
                    .then(|| {
                        address(
                            column.not_null_name.as_deref(),
                            column.not_null_identity.map(|identity| identity.oid),
                            None,
                        )
                    })
                    .flatten(),
                column.check.as_ref().and_then(|_| {
                    address(
                        column.check_name.as_deref(),
                        column.check_catalog_oid,
                        column.check_object_id,
                    )
                }),
                column.references.as_ref().and_then(|reference| {
                    address(
                        reference.name.as_deref(),
                        reference.catalog_identity.map(|identity| identity.oid),
                        None,
                    )
                }),
            ]
            .into_iter()
            .flatten()
        })
        .chain(checks.iter().filter_map(move |check| {
            address(check.name.as_deref(), check.catalog_oid, check.object_id)
        }))
        .chain(keys.iter().filter_map(move |key| {
            address(
                key.name.as_deref(),
                key.catalog_identity.map(|identity| identity.oid),
                None,
            )
        }))
        .chain(foreign_keys.iter().filter_map(move |key| {
            address(
                key.name.as_deref(),
                key.catalog_identity.map(|identity| identity.oid),
                None,
            )
        }))
        .chain(
            columns
                .iter()
                .filter_map(|column| column.references.as_ref())
                .flat_map(|reference| &reference.referenced_partitions)
                .chain(
                    foreign_keys
                        .iter()
                        .flat_map(|key| &key.referenced_partitions),
                )
                .map(|derived| derived.catalog_identity.oid),
        )
}
