//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve materialized foreign keys by name initially and by durable identity after a wait.

use super::{find_constraint, ConstraintLocation};
use crate::{
    ast::{ColumnDef, TableConstraintSet},
    SQLError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignKeyTarget<'a> {
    pub location: ConstraintLocation,
    pub name: &'a str,
    pub object_id: [u8; 16],
    pub referenced_table: &'a str,
}

impl<'a> ForeignKeyTarget<'a> {
    pub fn by_name(
        columns: &'a [ColumnDef],
        constraints: &'a TableConstraintSet,
        name: &str,
    ) -> Result<Option<Self>, SQLError> {
        find_constraint(columns, constraints, name).map_or(Ok(None), |location| {
            Self::at(columns, constraints, location)
        })
    }

    pub fn by_id(
        columns: &'a [ColumnDef],
        constraints: &'a TableConstraintSet,
        object_id: [u8; 16],
    ) -> Result<Option<Self>, SQLError> {
        let location = columns
            .iter()
            .position(|column| {
                column
                    .references
                    .as_ref()
                    .is_some_and(|reference| reference.object_id == Some(object_id))
            })
            .map(ConstraintLocation::ColumnForeignKey)
            .or_else(|| {
                constraints
                    .foreign_keys
                    .iter()
                    .position(|reference| reference.object_id == Some(object_id))
                    .map(ConstraintLocation::TableForeignKey)
            });
        location.map_or(Ok(None), |location| {
            Self::at(columns, constraints, location)
        })
    }

    fn at(
        columns: &'a [ColumnDef],
        constraints: &'a TableConstraintSet,
        location: ConstraintLocation,
    ) -> Result<Option<Self>, SQLError> {
        let (name, object_id, referenced_table) = match location {
            ConstraintLocation::ColumnForeignKey(index) => {
                let reference = columns[index]
                    .references
                    .as_ref()
                    .ok_or_else(|| SQLError::Internal("column FOREIGN KEY disappeared".into()))?;
                (
                    reference.name.as_deref(),
                    reference.object_id,
                    reference.table.as_str(),
                )
            }
            ConstraintLocation::TableForeignKey(index) => {
                let reference = &constraints.foreign_keys[index];
                (
                    reference.name.as_deref(),
                    reference.object_id,
                    reference.ref_table.as_str(),
                )
            }
            _ => return Ok(None),
        };
        Ok(Some(Self {
            location,
            name: name
                .ok_or_else(|| SQLError::Internal("FOREIGN KEY has no durable name".into()))?,
            object_id: object_id
                .ok_or_else(|| SQLError::Internal("FOREIGN KEY has no durable identity".into()))?,
            referenced_table,
        }))
    }
}

#[cfg(test)]
mod tests;
