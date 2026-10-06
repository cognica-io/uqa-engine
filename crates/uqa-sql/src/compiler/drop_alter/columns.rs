//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep complete column key declarations until schema publication assigns their identities.

use crate::{ast::AlterTableAction, SQLError};
use pg_query::{protobuf::AlterTableCmd, NodeEnum};

pub(super) fn add_column(command: &AlterTableCmd) -> Result<AlterTableAction, SQLError> {
    let definition = command
        .def
        .as_deref()
        .and_then(|node| node.node.as_ref())
        .ok_or_else(|| SQLError::Internal("ADD COLUMN without ColumnDef".into()))?;
    let NodeEnum::ColumnDef(column) = definition else {
        return Err(SQLError::Internal(format!(
            "ADD COLUMN expected ColumnDef, got {definition:?}"
        )));
    };
    let declaration = crate::compiler::tree::compile_column_declaration(column)?;
    let ty = column
        .type_name
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("column `{}` has no type", column.colname)))?;
    let ty = crate::compiler::types::preserve_alter_type_declaration(ty, &column.colname)?;
    let (definition, checks, foreign_keys) =
        crate::compiler::tree::compile_column_def_with_type(column, ty)?;
    Ok(AlterTableAction::AddColumn {
        column: definition,
        checks,
        foreign_keys,
        key_constraints: crate::compiler::tree::compile_column_key_constraints(column)?,
        if_not_exists: command.missing_ok,
        declaration,
    })
}
