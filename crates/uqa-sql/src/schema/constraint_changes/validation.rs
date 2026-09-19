//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select constraint validation dependencies and match inherited NOT NULL constraints by column.

use super::{constraint_error, find_constraint, ConstraintLocation};
use crate::{
    ast::{ColumnDef, TableConstraintSet},
    SQLError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintValidationKind<'a> {
    Check { no_inherit: bool },
    NotNull { column: &'a str, no_inherit: bool },
    ForeignKey { referenced_table: &'a str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConstraintValidation<'a> {
    pub kind: ConstraintValidationKind<'a>,
    pub validated: bool,
}

impl ConstraintValidation<'_> {
    pub fn requires_descendants(self) -> bool {
        !self.validated
            && matches!(
                self.kind,
                ConstraintValidationKind::Check { no_inherit: false }
                    | ConstraintValidationKind::NotNull {
                        no_inherit: false,
                        ..
                    }
            )
    }

    pub fn child_constraint_name<'a>(
        self,
        original: &'a str,
        child: &str,
        columns: &'a [ColumnDef],
    ) -> Result<&'a str, SQLError> {
        let ConstraintValidationKind::NotNull { column, .. } = self.kind else {
            return Ok(original);
        };
        super::inheritance::not_null_constraint(columns, column)
            .and_then(|candidate| candidate.not_null_name.as_deref())
            .ok_or_else(|| constraint_error("XX000", format!("cache lookup failed for not-null constraint on column \"{column}\" of relation \"{child}\"")))
    }
}

pub fn constraint_validation<'a>(
    table: &str,
    name: &str,
    columns: &'a [ColumnDef],
    constraints: &'a TableConstraintSet,
) -> Result<ConstraintValidation<'a>, SQLError> {
    let location = find_constraint(columns, constraints, name).ok_or_else(|| {
        constraint_error(
            "42704",
            format!("constraint \"{name}\" of relation \"{table}\" does not exist"),
        )
    })?;
    let (kind, validated, enforced) = match location {
        ConstraintLocation::NotNull(index) => {
            let column = &columns[index];
            (ConstraintValidationKind::NotNull { column: &column.name, no_inherit: column.not_null_no_inherit }, column.not_null_validated, true)
        }
        ConstraintLocation::ColumnCheck(index) => {
            let column = &columns[index];
            (ConstraintValidationKind::Check { no_inherit: column.check_no_inherit }, column.check_validated, column.check_enforced)
        }
        ConstraintLocation::TableCheck(index) => {
            let check = &constraints.checks[index];
            (ConstraintValidationKind::Check { no_inherit: check.no_inherit }, check.validated, check.enforced)
        }
        ConstraintLocation::ColumnForeignKey(index) => {
            let reference = columns[index].references.as_ref()
                .ok_or_else(|| SQLError::Internal("column FOREIGN KEY disappeared".into()))?;
            (ConstraintValidationKind::ForeignKey { referenced_table: &reference.table }, reference.validated, reference.enforced)
        }
        ConstraintLocation::TableForeignKey(index) => {
            let reference = &constraints.foreign_keys[index];
            (ConstraintValidationKind::ForeignKey { referenced_table: &reference.ref_table }, reference.validated, reference.enforced)
        }
        ConstraintLocation::Key(_) => return Err(constraint_error("42809", format!("constraint \"{name}\" of relation \"{table}\" is not a foreign key, check, or not-null constraint"))),
    };
    if !enforced {
        return Err(constraint_error(
            "55000",
            "cannot validate NOT ENFORCED constraint",
        ));
    }
    Ok(ConstraintValidation { kind, validated })
}

pub fn ensure_validation_recurses(recurse: bool, has_children: bool) -> Result<(), SQLError> {
    if !recurse && has_children {
        return Err(constraint_error(
            "42P16",
            "constraint must be validated on child tables too",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
