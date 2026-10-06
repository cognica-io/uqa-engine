//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation attribute numbers and the physical metadata retained by dropped slots.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::type_metadata;
use crate::ast::{AutoIncrementKind, ColumnDef, ColumnType};
use crate::SQLError;

/// `PostgreSQL`'s `MaxHeapAttributeNumber`, including dropped attributes.
pub const MAX_ATTRIBUTES: i16 = 1600;

/// A dropped attribute has no live name, type dependency, default or ACL. Its physical type metadata survives even when the old type is subsequently removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DroppedAttribute {
    pub number: i16,
    pub type_length: i64,
    pub type_modifier: i64,
    pub dimensions: i64,
    pub by_value: bool,
    pub alignment: String,
    pub storage: String,
    pub collation: i64,
    pub identity: String,
    pub is_local: bool,
    pub inheritance_count: i64,
}

impl DroppedAttribute {
    pub fn from_column(
        column: &ColumnDef,
        position: usize,
        is_local: bool,
        inheritance_count: i64,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            number: column_number(column, position)?,
            type_length: type_metadata::pg_type_len(&column.ty),
            type_modifier: type_metadata::pg_type_modifier(&column.ty),
            dimensions: array_dimension_count(&column.ty),
            by_value: type_metadata::pg_type_by_value(&column.ty),
            alignment: type_metadata::pg_type_align(&column.ty).into(),
            storage: type_metadata::pg_type_storage(&column.ty).into(),
            collation: type_metadata::pg_type_collation_oid(&column.ty),
            identity: match column.auto_increment.as_ref().map(|value| value.kind) {
                Some(AutoIncrementKind::IdentityAlways) => "a",
                Some(AutoIncrementKind::IdentityByDefault | AutoIncrementKind::Legacy) => "d",
                Some(AutoIncrementKind::Serial) | None => "",
            }
            .into(),
            is_local,
            inheritance_count,
        })
    }

    pub fn name(&self) -> String {
        super::composite_type::StoredCompositeAttribute::dropped_name(self.number)
    }
}

/// Stored columns use their durable number. Unpublished declarations and derived view/index schemas have their own consecutive positions.
pub fn column_number(column: &ColumnDef, position: usize) -> Result<i16, SQLError> {
    let number = match column.attribute_number {
        Some(number) => number,
        None => i16::try_from(position + 1)
            .map_err(|_| SQLError::Internal("attribute number is out of range".into()))?,
    };
    if number <= 0 {
        return Err(SQLError::Internal(
            "attribute number must be positive".into(),
        ));
    }
    Ok(number)
}

pub fn column_names(columns: &[ColumnDef]) -> Result<Vec<(i16, String)>, SQLError> {
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| Ok((column_number(column, index)?, column.name.clone())))
        .collect()
}

pub fn column_by_number(columns: &[ColumnDef], number: i64) -> Option<&ColumnDef> {
    columns.iter().enumerate().find_map(|(index, column)| {
        column_number(column, index)
            .is_ok_and(|candidate| i64::from(candidate) == number)
            .then_some(column)
    })
}

pub fn consecutive_names(names: Vec<String>) -> Result<Vec<(i16, String)>, SQLError> {
    names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            Ok((
                i16::try_from(index + 1)
                    .map_err(|_| SQLError::Internal("attribute number is out of range".into()))?,
                name,
            ))
        })
        .collect()
}

pub fn array_dimension_count(ty: &ColumnType) -> i64 {
    let mut dimensions = 0;
    let mut current = ty;
    while let ColumnType::Array(element) = current {
        dimensions += 1;
        current = element;
    }
    dimensions
}

/// A schema candidate retains numbers only from this relation's existing columns. Inherited or newly declared columns receive fresh local numbers when published.
pub fn retain_numbers(previous: &[ColumnDef], candidate: &mut [ColumnDef]) {
    for column in candidate {
        column.attribute_number = previous
            .iter()
            .find(|prior| {
                column
                    .object_id
                    .map_or(prior.name == column.name, |identity| {
                        prior.object_id == Some(identity)
                    })
            })
            .and_then(|prior| prior.attribute_number);
    }
}

/// Assign numbers only to newly published columns, after every live or dropped slot. Existing numbers and the inputs remain unchanged on an invalid layout.
pub fn materialize(
    columns: &mut [ColumnDef],
    dropped: &[DroppedAttribute],
) -> Result<bool, SQLError> {
    let mut maximum = columns
        .iter()
        .filter_map(|column| column.attribute_number)
        .chain(dropped.iter().map(|attribute| attribute.number))
        .max()
        .unwrap_or(0);
    let mut numbers = Vec::with_capacity(columns.len());
    let mut changed = false;
    for column in columns.iter() {
        numbers.push(if let Some(number) = column.attribute_number {
            number
        } else {
            maximum = maximum.checked_add(1).ok_or_else(too_many_columns)?;
            changed = true;
            maximum
        });
    }
    validate_numbers(&numbers, dropped)?;
    for (column, number) in columns.iter_mut().zip(numbers) {
        column.attribute_number = Some(number);
    }
    Ok(changed)
}

/// Load-only restoration rejects missing or contradictory metadata instead of repairing a catalog snapshot.
pub fn validate(columns: &[ColumnDef], dropped: &[DroppedAttribute]) -> Result<(), SQLError> {
    let numbers = columns
        .iter()
        .map(|column| {
            column.attribute_number.ok_or_else(|| {
                SQLError::Internal("relation attributes require initial catalog migration".into())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_numbers(&numbers, dropped)
}

fn validate_numbers(numbers: &[i16], dropped: &[DroppedAttribute]) -> Result<(), SQLError> {
    let all: BTreeSet<_> = numbers
        .iter()
        .copied()
        .chain(dropped.iter().map(|attribute| attribute.number))
        .collect();
    if all.last().is_some_and(|number| *number > MAX_ATTRIBUTES) {
        return Err(too_many_columns());
    }
    if all.len() != numbers.len() + dropped.len()
        || all
            .iter()
            .copied()
            .ne(1..=i16::try_from(all.len()).unwrap_or(i16::MAX))
        || numbers.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(SQLError::Internal(
            "invalid relation attribute slot layout".into(),
        ));
    }
    Ok(())
}

fn too_many_columns() -> SQLError {
    SQLError::Routine {
        sqlstate: "54011".into(),
        message: format!("tables can have at most {MAX_ATTRIBUTES} columns"),
    }
}

#[cfg(test)]
mod tests;
