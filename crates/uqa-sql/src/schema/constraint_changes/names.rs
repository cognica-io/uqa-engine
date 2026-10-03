//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Names and independent catalog identities from a borrowed relation declaration.

use super::{ConstraintLocation, ForeignKeyLocation};
use crate::ast::{
    ColumnDef, ForeignKey, ReferencedPartitionConstraint, TableCheck, TableConstraintSet,
    TableKeyConstraint,
};

#[derive(Clone, Copy)]
pub struct NamedConstraint<'a> {
    pub name: &'a str,
    pub object_id: Option<[u8; 16]>,
    pub location: ConstraintLocation,
}

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub struct ConstraintNames<'a> {
    pub columns: &'a [ColumnDef],
    pub checks: &'a [TableCheck],
    pub foreign_keys: &'a [ForeignKey],
    pub keys: &'a [TableKeyConstraint],
}

impl<'a> ConstraintNames<'a> {
    pub fn from_definition(columns: &'a [ColumnDef], constraints: &'a TableConstraintSet) -> Self {
        Self {
            columns,
            checks: &constraints.checks,
            foreign_keys: &constraints.foreign_keys,
            keys: &constraints.key_constraints,
        }
    }

    pub fn entries(self) -> impl Iterator<Item = NamedConstraint<'a>> {
        let not_null = self
            .columns
            .iter()
            .enumerate()
            .filter_map(|(position, column)| {
                column.not_null.then_some(())?;
                Some(NamedConstraint {
                    name: column.not_null_name.as_deref()?,
                    object_id: column.not_null_identity.map(|id| id.object_id),
                    location: ConstraintLocation::NotNull(position),
                })
            });
        let column_checks = self
            .columns
            .iter()
            .enumerate()
            .filter_map(|(position, column)| {
                column.check.as_ref()?;
                Some(NamedConstraint {
                    name: column.check_name.as_deref()?,
                    object_id: column.check_object_id,
                    location: ConstraintLocation::ColumnCheck(position),
                })
            });
        let column_foreign_keys =
            self.columns
                .iter()
                .enumerate()
                .filter_map(|(position, column)| {
                    let key = column.references.as_ref()?;
                    Some(NamedConstraint {
                        name: key.name.as_deref()?,
                        object_id: key.catalog_identity.map(|id| id.object_id),
                        location: ConstraintLocation::ColumnForeignKey(position),
                    })
                });
        let checks = self
            .checks
            .iter()
            .enumerate()
            .filter_map(|(position, check)| {
                Some(NamedConstraint {
                    name: check.name.as_deref()?,
                    object_id: check.object_id,
                    location: ConstraintLocation::TableCheck(position),
                })
            });
        let foreign_keys = self
            .foreign_keys
            .iter()
            .enumerate()
            .filter_map(|(position, key)| {
                Some(NamedConstraint {
                    name: key.name.as_deref()?,
                    object_id: key.catalog_identity.map(|id| id.object_id),
                    location: ConstraintLocation::TableForeignKey(position),
                })
            });
        let keys = self.keys.iter().enumerate().filter_map(|(position, key)| {
            Some(NamedConstraint {
                name: key.name.as_deref()?,
                object_id: key.catalog_identity.map(|id| id.object_id),
                location: ConstraintLocation::Key(position),
            })
        });
        let column_derived = self
            .columns
            .iter()
            .enumerate()
            .filter_map(|(position, column)| Some((position, column.references.as_ref()?)))
            .flat_map(|(position, reference)| {
                derived_entries(
                    ForeignKeyLocation::Column(position),
                    &reference.referenced_partitions,
                )
            });
        let table_derived =
            self.foreign_keys
                .iter()
                .enumerate()
                .flat_map(|(position, foreign_key)| {
                    derived_entries(
                        ForeignKeyLocation::Table(position),
                        &foreign_key.referenced_partitions,
                    )
                });
        not_null
            .chain(column_checks)
            .chain(column_foreign_keys)
            .chain(checks)
            .chain(foreign_keys)
            .chain(keys)
            .chain(column_derived)
            .chain(table_derived)
    }
}

/// The derived constraints of one foreign key, which share the relation's constraint names.
fn derived_entries(
    foreign_key: ForeignKeyLocation,
    constraints: &[ReferencedPartitionConstraint],
) -> impl Iterator<Item = NamedConstraint<'_>> {
    constraints
        .iter()
        .enumerate()
        .map(move |(index, constraint)| NamedConstraint {
            name: &constraint.name,
            object_id: Some(constraint.catalog_identity.object_id),
            location: ConstraintLocation::ReferencedPartition(foreign_key, index),
        })
}
