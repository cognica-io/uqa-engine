//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` ALTER execution categories; written order is retained within each category.

use crate::ast::AlterTableAction;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlterOrder {
    Drop,
    Type,
    AddColumn,
    SetExpression,
    AddConstraint,
    ColumnAttributes,
    AddKey,
    AddDefault,
    Other,
}

pub fn execution_order(action: &AlterTableAction) -> AlterOrder {
    match action {
        AlterTableAction::DropColumn { .. }
        | AlterTableAction::DropConstraint { .. }
        | AlterTableAction::DropDefault { .. }
        | AlterTableAction::DropNotNull { .. }
        | AlterTableAction::DropIdentity { .. }
        | AlterTableAction::DropExpression { .. } => AlterOrder::Drop,
        AlterTableAction::AlterColumnType { .. } => AlterOrder::Type,
        AlterTableAction::AddColumn { .. } => AlterOrder::AddColumn,
        AlterTableAction::SetExpression { .. } => AlterOrder::SetExpression,
        AlterTableAction::AddCheckConstraint { .. }
        | AlterTableAction::AddNotNullConstraint { .. }
        | AlterTableAction::AddForeignKeyConstraint { .. } => AlterOrder::AddConstraint,
        AlterTableAction::SetNotNull { .. } => AlterOrder::ColumnAttributes,
        AlterTableAction::AddKeyConstraint { .. } => AlterOrder::AddKey,
        AlterTableAction::SetDefault { .. } | AlterTableAction::AddIdentity { .. } => {
            AlterOrder::AddDefault
        }
        _ => AlterOrder::Other,
    }
}
