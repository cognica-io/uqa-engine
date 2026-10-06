//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-kind permissions for ALTER TABLE commands addressed to views.

use crate::{
    ast::{AlterTableAction, EventEnableMode, RelationPersistence},
    SQLError,
};

pub(super) fn validate_actions(
    local_name: &str,
    kind: &str,
    actions: &[AlterTableAction],
) -> Result<(), SQLError> {
    for action in actions {
        if let Some(command) = forbidden_command(action, kind == "materialized view") {
            return Err(SQLError::Diagnostic {
                sqlstate: "42809".into(),
                message: format!(
                    "ALTER action {command} cannot be performed on relation \"{local_name}\""
                ),
                detail: Some(format!("This operation is not supported for {kind}s.")),
                hint: None,
            });
        }
    }
    Ok(())
}

/// Match `PostgreSQL`'s `ATSimplePermissions` targets and `alter_table_type_to_string` labels.
fn forbidden_command(action: &AlterTableAction, materialized: bool) -> Option<&'static str> {
    use AlterTableAction as Action;
    Some(match action {
        Action::AddColumn { .. } => "ADD COLUMN",
        Action::DropColumn { .. } => "DROP COLUMN",
        Action::SetNotNull { .. } => "ALTER COLUMN ... SET NOT NULL",
        Action::DropNotNull { .. } => "ALTER COLUMN ... DROP NOT NULL",
        Action::SetExpression { .. } => "ALTER COLUMN ... SET EXPRESSION",
        Action::DropExpression { .. } => "ALTER COLUMN ... DROP EXPRESSION",
        Action::AlterColumnType { .. } => "ALTER COLUMN ... SET DATA TYPE",
        Action::AddKeyConstraint { .. }
        | Action::AddCheckConstraint { .. }
        | Action::AddForeignKeyConstraint { .. }
        | Action::AddNotNullConstraint { .. } => "ADD CONSTRAINT",
        Action::DropConstraint { .. } => "DROP CONSTRAINT",
        Action::AlterConstraint { .. } => "ALTER CONSTRAINT",
        Action::ValidateConstraint { .. } => "VALIDATE CONSTRAINT",
        Action::AddInheritance { .. } => "INHERIT",
        Action::DropInheritance { .. } => "NO INHERIT",
        Action::AttachPartition { .. } => "ATTACH PARTITION",
        Action::DetachPartition { finalize: true, .. } => "DETACH PARTITION ... FINALIZE",
        Action::DetachPartition { .. } => "DETACH PARTITION",
        Action::SetPersistence {
            persistence: RelationPersistence::Permanent,
        } => "SET LOGGED",
        Action::SetPersistence {
            persistence: RelationPersistence::Unlogged,
        } => "SET UNLOGGED",
        Action::SetTriggerEnableMode {
            name,
            user_only,
            mode,
        } => match (mode, name.is_some(), user_only) {
            (EventEnableMode::Replica, _, _) => "ENABLE REPLICA TRIGGER",
            (EventEnableMode::Always, _, _) => "ENABLE ALWAYS TRIGGER",
            (EventEnableMode::Origin, true, _) => "ENABLE TRIGGER",
            (EventEnableMode::Disabled, true, _) => "DISABLE TRIGGER",
            (EventEnableMode::Origin, false, true) => "ENABLE TRIGGER USER",
            (EventEnableMode::Disabled, false, true) => "DISABLE TRIGGER USER",
            (EventEnableMode::Origin, false, false) => "ENABLE TRIGGER ALL",
            (EventEnableMode::Disabled, false, false) => "DISABLE TRIGGER ALL",
        },
        Action::SetRuleEnableMode { mode, .. } => match mode {
            EventEnableMode::Origin => "ENABLE RULE",
            EventEnableMode::Disabled => "DISABLE RULE",
            EventEnableMode::Replica => "ENABLE REPLICA RULE",
            EventEnableMode::Always => "ENABLE ALWAYS RULE",
        },
        Action::SetDefault { .. } | Action::DropDefault { .. } if materialized => {
            "ALTER COLUMN ... SET DEFAULT"
        }
        Action::AddIdentity { .. } if materialized => "ALTER COLUMN ... ADD IDENTITY",
        Action::SetIdentity { .. } if materialized => "ALTER COLUMN ... SET",
        Action::DropIdentity { .. } if materialized => "ALTER COLUMN ... DROP IDENTITY",
        // These use a separate grammar path, or accept a regular view in ATSimplePermissions.
        Action::RenameColumn { .. }
        | Action::RenameConstraint { .. }
        | Action::RenameTable { .. }
        | Action::RenameTrigger { .. }
        | Action::RenameRule { .. }
        | Action::ChangeOwner { .. }
        | Action::SetSchema { .. }
        | Action::SetDefault { .. }
        | Action::DropDefault { .. }
        | Action::AddIdentity { .. }
        | Action::SetIdentity { .. }
        | Action::DropIdentity { .. }
        | Action::SetPersistence {
            persistence: RelationPersistence::Temporary,
        } => return None,
    })
}
