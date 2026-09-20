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
    let (definition, checks) = crate::compiler::tree::compile_column_def(column)?;
    Ok(AlterTableAction::AddColumn {
        column: definition,
        checks,
        key_constraints: crate::compiler::tree::compile_column_key_constraints(column)?,
        if_not_exists: command.missing_ok,
    })
}
