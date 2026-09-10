//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind inherited ALTER declarations and their durable constraint identities.
use super::constraint_metadata::CatalogIdentityAllocator;
use crate::{
    ast::{AlterTableAction, ColumnDef, TableConstraintSet},
    SQLError,
};
use uqa_core::RelationIdentity;

pub fn normalize_inherited_action(action: &mut AlterTableAction) {
    if let AlterTableAction::AddColumn { column, .. } = action {
        column.not_null_is_local = !column.not_null;
        column.check_is_local = column.check.is_none();
        column.check_object_id = None;
        if column.check_no_inherit {
            column.check = None;
            column.check_name = None;
            column.check_is_local = true;
            column.check_no_inherit = false;
        }
    }
    if let AlterTableAction::AddCheckConstraint { constraint } = action {
        constraint.is_local = false;
        constraint.object_id = None;
    }
}

pub fn materialize_recursive_action_names(
    relation: &RelationIdentity,
    columns: &mut Vec<ColumnDef>,
    constraints: &mut TableConstraintSet,
    action: &mut AlterTableAction,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    match action {
        AlterTableAction::AddColumn { column, .. } => {
            columns.push(column.clone());
            super::constraint_metadata::materialize_constraint_metadata(
                relation,
                columns,
                constraints,
                allocate,
            )
            .map_err(|error| {
                crate::catalog::errors::storage_error("ALTER TABLE ADD COLUMN", &error)
            })?;
            *column = columns
                .pop()
                .ok_or_else(|| SQLError::Internal("new column disappeared".into()))?;
        }
        AlterTableAction::AddCheckConstraint { constraint }
            if !constraint.no_inherit && constraint.name.is_none() =>
        {
            constraints.checks.push(constraint.clone());
            super::constraint_metadata::materialize_constraint_metadata(
                relation,
                columns,
                constraints,
                allocate,
            )
            .map_err(|error| {
                crate::catalog::errors::storage_error("ALTER TABLE ADD CONSTRAINT", &error)
            })?;
            *constraint = constraints
                .checks
                .pop()
                .ok_or_else(|| SQLError::Internal("new CHECK constraint disappeared".into()))?;
        }
        AlterTableAction::AddNotNullConstraint {
            name,
            column,
            validated,
            no_inherit: false,
        } if name.is_none() => {
            if let Some(definition) = columns
                .iter_mut()
                .find(|definition| definition.name == *column && !definition.not_null)
            {
                definition.not_null = true;
                definition.not_null_explicit = true;
                definition.not_null_validated = *validated;
                super::constraint_metadata::materialize_constraint_metadata(
                    relation,
                    columns,
                    constraints,
                    allocate,
                )
                .map_err(|error| {
                    crate::catalog::errors::storage_error("ALTER TABLE ADD CONSTRAINT", &error)
                })?;
                *name = columns
                    .iter()
                    .find(|definition| definition.name == *column)
                    .and_then(|definition| definition.not_null_name.clone());
            }
        }
        _ => {}
    }
    Ok(())
}

pub mod syntax;
