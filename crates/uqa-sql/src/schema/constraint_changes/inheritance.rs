//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Match inherited constraints and select removal behavior from their remaining origins.

use super::{constraint_error, find_constraint, ConstraintLocation};
use crate::{
    ast::{ColumnDef, TableConstraintSet},
    SQLError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InheritedConstraintKey<'a> {
    Check(&'a str),
    NotNull(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InheritedConstraint<'a> {
    pub name: &'a str,
    pub key: InheritedConstraintKey<'a>,
    pub no_inherit: bool,
    pub is_local: bool,
}

impl<'a> InheritedConstraint<'a> {
    pub fn find(
        columns: &'a [ColumnDef],
        constraints: &'a TableConstraintSet,
        name: &'a str,
    ) -> Option<Self> {
        let (key, no_inherit, is_local) = match find_constraint(columns, constraints, name)? {
            ConstraintLocation::NotNull(index) => {
                let column = &columns[index];
                (
                    InheritedConstraintKey::NotNull(column.name.as_str()),
                    column.not_null_no_inherit,
                    column.not_null_is_local,
                )
            }
            ConstraintLocation::ColumnCheck(index) => {
                let column = &columns[index];
                (
                    InheritedConstraintKey::Check(name),
                    column.check_no_inherit,
                    column.check_is_local,
                )
            }
            ConstraintLocation::TableCheck(index) => {
                let check = &constraints.checks[index];
                (
                    InheritedConstraintKey::Check(name),
                    check.no_inherit,
                    check.is_local,
                )
            }
            _ => return None,
        };
        Some(Self {
            name,
            key,
            no_inherit,
            is_local,
        })
    }
}

pub fn not_null_constraint<'a>(columns: &'a [ColumnDef], name: &str) -> Option<&'a ColumnDef> {
    columns
        .iter()
        .find(|column| column.name == name && column.not_null)
}

impl InheritedConstraintKey<'_> {
    pub fn find<'a>(
        self,
        columns: &'a [ColumnDef],
        constraints: &'a TableConstraintSet,
    ) -> Option<InheritedConstraint<'a>> {
        let name = match self {
            Self::NotNull(column) => not_null_constraint(columns, column)?
                .not_null_name
                .as_deref()?,
            Self::Check(name) => match find_constraint(columns, constraints, name)? {
                ConstraintLocation::ColumnCheck(index) => columns[index].check_name.as_deref()?,
                ConstraintLocation::TableCheck(index) => {
                    constraints.checks[index].name.as_deref()?
                }
                _ => return None,
            },
        };
        InheritedConstraint::find(columns, constraints, name)
    }

    pub fn require<'a>(
        self,
        table: &str,
        columns: &'a [ColumnDef],
        constraints: &'a TableConstraintSet,
    ) -> Result<InheritedConstraint<'a>, SQLError> {
        self.find(columns, constraints).ok_or_else(|| match self {
            Self::Check(name) => constraint_error("42704", format!("constraint \"{name}\" of relation \"{table}\" does not exist")),
            Self::NotNull(column) => constraint_error("XX000", format!("cache lookup failed for not-null constraint on column \"{column}\" of relation \"{table}\"")),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InheritedConstraintRemoval {
    Drop,
    Keep,
    MakeLocal,
}

pub fn inherited_constraint_removal(
    recurse: bool,
    is_local: bool,
    remaining_parents: usize,
) -> InheritedConstraintRemoval {
    if recurse && !is_local && remaining_parents == 0 {
        InheritedConstraintRemoval::Drop
    } else if !recurse && remaining_parents == 0 && !is_local {
        InheritedConstraintRemoval::MakeLocal
    } else {
        InheritedConstraintRemoval::Keep
    }
}

pub fn ensure_inherited_constraint_removable(
    table: &str,
    name: &str,
    parents: usize,
) -> Result<(), SQLError> {
    if parents == 0 {
        return Ok(());
    }
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    Err(constraint_error(
        "42P16",
        format!(
            "cannot drop inherited constraint \"{name}\" of relation \"{}\"",
            relation.name
        ),
    ))
}

pub fn make_constraint_local(
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    name: &str,
) -> Result<(), SQLError> {
    match find_constraint(columns, constraints, name) {
        Some(ConstraintLocation::NotNull(index)) => columns[index].not_null_is_local = true,
        Some(ConstraintLocation::ColumnCheck(index)) => columns[index].check_is_local = true,
        Some(ConstraintLocation::TableCheck(index)) => constraints.checks[index].is_local = true,
        _ => {
            return Err(SQLError::Internal(format!(
                "inherited constraint \"{name}\" disappeared"
            )))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
