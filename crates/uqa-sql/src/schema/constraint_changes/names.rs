//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Names and independent catalog identities from a borrowed relation declaration.

use super::ConstraintLocation;
use crate::ast::{ColumnDef, ForeignKey, TableCheck, TableConstraintSet, TableKeyConstraint};

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
        not_null
            .chain(column_checks)
            .chain(column_foreign_keys)
            .chain(checks)
            .chain(foreign_keys)
            .chain(keys)
    }
}
