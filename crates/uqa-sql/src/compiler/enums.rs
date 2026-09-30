//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum type declarations and label alterations.

use pg_query::protobuf::{AlterEnumStmt, CreateEnumStmt};

use super::{domains::qualified_name, extract_string, Result};
use crate::ast::{AlterEnum, AlterEnumAction, CreateEnum, EnumNeighbor};

pub(super) fn compile_create_enum(statement: &CreateEnumStmt) -> Result<CreateEnum> {
    Ok(CreateEnum {
        name: qualified_name(&statement.type_name)?,
        labels: statement
            .vals
            .iter()
            .map(extract_string)
            .collect::<Result<Vec<_>>>()?,
    })
}

/// The parser encodes `RENAME VALUE` with a nonempty old label; otherwise the statement adds `new_val`.
pub(super) fn compile_alter_enum(statement: &AlterEnumStmt) -> Result<AlterEnum> {
    let action = if statement.old_val.is_empty() {
        AlterEnumAction::AddValue {
            label: statement.new_val.clone(),
            if_not_exists: statement.skip_if_new_val_exists,
            neighbor: (!statement.new_val_neighbor.is_empty()).then(|| EnumNeighbor {
                label: statement.new_val_neighbor.clone(),
                after: statement.new_val_is_after,
            }),
        }
    } else {
        AlterEnumAction::RenameValue {
            old: statement.old_val.clone(),
            new: statement.new_val.clone(),
        }
    };
    Ok(AlterEnum {
        name: qualified_name(&statement.type_name)?,
        action,
    })
}
