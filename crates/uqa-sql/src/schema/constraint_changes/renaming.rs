//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inherited constraint rename diagnostics and metadata edits preserve constraint identity.

use super::{
    constraint_error, ensure_constraint_name_available, find_constraint, ConstraintLocation,
};
use crate::{
    ast::{ColumnDef, TableConstraintSet},
    SQLError,
};

pub fn ensure_recursive_rename(
    name: &str,
    recurse: bool,
    has_children: bool,
) -> Result<(), SQLError> {
    if !recurse && has_children {
        return Err(constraint_error(
            "42P16",
            format!("inherited constraint \"{name}\" must be renamed in child tables too"),
        ));
    }
    Ok(())
}

pub fn ensure_rename_parents(name: &str, parents: usize, expected: usize) -> Result<(), SQLError> {
    if parents > expected {
        return Err(constraint_error(
            "42P16",
            format!("cannot rename inherited constraint \"{name}\""),
        ));
    }
    Ok(())
}

pub fn rename_inherited_constraint(
    table: &str,
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    from: &str,
    to: &str,
) -> Result<(), SQLError> {
    let location = find_constraint(columns, constraints, from)
        .filter(|location| {
            matches!(
                location,
                ConstraintLocation::NotNull(_)
                    | ConstraintLocation::ColumnCheck(_)
                    | ConstraintLocation::TableCheck(_)
            )
        })
        .ok_or_else(|| {
            constraint_error(
                "42704",
                format!("constraint \"{from}\" for table \"{table}\" does not exist"),
            )
        })?;
    ensure_constraint_name_available(columns, constraints, Some(to), table)?;
    match location {
        ConstraintLocation::NotNull(index) => columns[index].not_null_name = Some(to.to_string()),
        ConstraintLocation::ColumnCheck(index) => columns[index].check_name = Some(to.to_string()),
        ConstraintLocation::TableCheck(index) => {
            constraints.checks[index].name = Some(to.to_string());
        }
        _ => unreachable!("only inherited constraint locations are selected"),
    }
    Ok(())
}

/// Foreign-key rename is local even for a partition parent or a partition clone. Enforcement and deferred-event identities remain unchanged.
pub fn rename_foreign_key(
    table: &str,
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    from: &str,
    to: &str,
) -> Result<bool, SQLError> {
    let Some(location) = find_constraint(columns, constraints, from).filter(|location| {
        matches!(
            location,
            ConstraintLocation::ColumnForeignKey(_) | ConstraintLocation::TableForeignKey(_)
        )
    }) else {
        return Ok(false);
    };
    ensure_constraint_name_available(columns, constraints, Some(to), table)?;
    let (name, identity) = match location {
        ConstraintLocation::ColumnForeignKey(index) => {
            let reference = columns[index]
                .references
                .as_mut()
                .expect("selected foreign key");
            (&mut reference.name, reference.catalog_identity)
        }
        ConstraintLocation::TableForeignKey(index) => {
            let reference = &mut constraints.foreign_keys[index];
            (&mut reference.name, reference.catalog_identity)
        }
        _ => unreachable!("only foreign-key locations are selected"),
    };
    let identity = identity
        .ok_or_else(|| SQLError::Internal("FOREIGN KEY has no durable catalog identity".into()))?;
    *name = Some(to.to_string());
    for inherited in &mut constraints.hierarchy.partition_inherited_foreign_keys {
        if inherited.catalog_identity == Some(identity) {
            inherited.name = Some(to.to_string());
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
