//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate ALTER TABLE transaction restrictions and bind native relation-kind actions.
use crate::{
    ast::{AlterTableAction, AlterTableStmt},
    SQLError,
};

pub fn validate_alter_table_transaction(
    stmt: &AlterTableStmt,
    in_transaction_block: bool,
) -> Result<(), SQLError> {
    if in_transaction_block
        && stmt.actions.iter().any(|action| {
            matches!(
                action,
                AlterTableAction::DetachPartition {
                    concurrently: true,
                    ..
                }
            )
        })
    {
        return Err(SQLError::Routine {
            sqlstate: "25001".into(),
            message: "ALTER TABLE ... DETACH CONCURRENTLY cannot run inside a transaction block"
                .into(),
        });
    }
    Ok(())
}

pub fn alter_sequence_from_table_syntax(
    canonical: &str,
    stmt: &AlterTableStmt,
) -> Result<crate::ast::AlterSequence, SQLError> {
    let mut alter = crate::ast::AlterSequence {
        name: canonical.to_string(),
        if_exists: stmt.if_exists,
        ..crate::ast::AlterSequence::default()
    };
    match stmt.actions.as_slice() {
        [AlterTableAction::SetPersistence { persistence }] => {
            alter.persistence = Some(*persistence);
        }
        [AlterTableAction::RenameTable { to }] => {
            alter.lifecycle = crate::ast::SequenceLifecycle::RenameTo { name: to.clone() };
        }
        [AlterTableAction::SetSchema { schema }] => {
            alter.lifecycle = crate::ast::SequenceLifecycle::SetSchema {
                schema: schema.clone(),
            };
        }
        [AlterTableAction::ChangeOwner { owner }] => {
            alter.role_owner = Some(owner.clone());
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("ALTER TABLE: relation `{canonical}` is a sequence, not a table"),
            });
        }
    }
    Ok(alter)
}

/// Return a native view rename, or None for a valid sequence of view event renames.
pub fn alter_view_from_table_syntax(
    canonical: &str,
    kind: &str,
    stmt: &AlterTableStmt,
) -> Result<Option<crate::ast::AlterViewStmt>, SQLError> {
    if let [AlterTableAction::RenameTable { to }] = stmt.actions.as_slice() {
        return Ok(Some(crate::ast::AlterViewStmt {
            name: canonical.to_string(),
            kind: if kind == "view" {
                crate::ast::AlterViewKind::View
            } else {
                crate::ast::AlterViewKind::MaterializedView
            },
            if_exists: stmt.if_exists,
            action: crate::ast::AlterViewAction::RenameTo(to.clone()),
        }));
    }
    if kind == "view"
        && stmt.actions.iter().all(|action| {
            matches!(
                action,
                AlterTableAction::RenameRule { .. } | AlterTableAction::RenameTrigger { .. }
            )
        })
    {
        return Ok(None);
    }
    Err(SQLError::Routine {
        sqlstate: "42809".into(),
        message: format!("ALTER TABLE: relation `{canonical}` is a {kind}, not a table"),
    })
}

/// Return a native foreign-table owner or name change, or None for valid trigger actions.
pub fn alter_foreign_table_from_table_syntax(
    canonical: &str,
    stmt: &AlterTableStmt,
) -> Result<Option<crate::ast::AlterForeignTableStmt>, SQLError> {
    if stmt.actions.iter().all(|action| {
        matches!(
            action,
            AlterTableAction::RenameTrigger { .. } | AlterTableAction::SetTriggerEnableMode { .. }
        )
    }) {
        return Ok(None);
    }
    let action = match stmt.actions.as_slice() {
        [AlterTableAction::ChangeOwner { owner }] => {
            crate::ast::AlterForeignTableAction::OwnerTo(owner.clone())
        }
        [AlterTableAction::RenameTable { to }] => {
            crate::ast::AlterForeignTableAction::RenameTo(to.clone())
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!(
                    "ALTER TABLE: relation `{canonical}` is a foreign table, not a table"
                ),
            });
        }
    };
    Ok(Some(crate::ast::AlterForeignTableStmt {
        name: canonical.to_string(),
        if_exists: stmt.if_exists,
        action,
    }))
}
