//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lower the `ALTER TABLE ... ALTER COLUMN` identity actions.

use crate::ast::{AlterTableAction, AutoIncrementKind, DeferredSQLError, SequenceDeclaration};
use crate::SQLError;
use pg_query::{protobuf::AlterTableCmd, NodeEnum};

/// `ADD GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY [ ( options ) ]`, whose options create the column's sequence as an identity declaration's do.
pub(super) fn add_identity(command: &AlterTableCmd) -> Result<AlterTableAction, SQLError> {
    let Some(NodeEnum::Constraint(constraint)) =
        command.def.as_deref().and_then(|node| node.node.as_ref())
    else {
        return Err(SQLError::Internal(
            "ADD GENERATED AS IDENTITY without its identity constraint".into(),
        ));
    };
    Ok(AlterTableAction::AddIdentity {
        name: command.name.clone(),
        kind: identity_kind(&constraint.generated_when)?,
        declaration: crate::compiler::sequences::compile_identity_declaration(&constraint.options)?,
    })
}

/// `SET GENERATED`, `RESTART` and `SET sequence_option`. `PostgreSQL` reads the sequence options as `ALTER SEQUENCE` does when it changes the sequence, so an error collecting them waits until then, and it finds a repeated `SET GENERATED` only after that change.
pub(super) fn set_identity(command: &AlterTableCmd) -> Result<AlterTableAction, SQLError> {
    let Some(NodeEnum::List(list)) = command.def.as_deref().and_then(|node| node.node.as_ref())
    else {
        return Err(SQLError::Internal(
            "ALTER COLUMN identity change without its option list".into(),
        ));
    };
    let mut kind = None;
    let mut repeated_kind = false;
    let mut options = Vec::with_capacity(list.items.len());
    for item in &list.items {
        let Some(NodeEnum::DefElem(elem)) = item.node.as_ref() else {
            return Err(SQLError::Internal(
                "ALTER COLUMN identity change contains a malformed option".into(),
            ));
        };
        if elem.defname != "generated" {
            options.push(elem.as_ref());
            continue;
        }
        let Some(NodeEnum::Integer(generation)) =
            elem.arg.as_deref().and_then(|node| node.node.as_ref())
        else {
            return Err(SQLError::Internal(
                "SET GENERATED without its generation".into(),
            ));
        };
        let generation = u8::try_from(generation.ival)
            .ok()
            .map(|byte| char::from(byte).to_string())
            .unwrap_or_default();
        if kind.is_some() {
            repeated_kind = true;
        } else {
            kind = Some(identity_kind(&generation)?);
        }
    }
    let (sequence, error) = match crate::compiler::sequences::collect_sequence_options(
        options,
        "ALTER SEQUENCE",
        false,
    ) {
        Ok(sequence) => (sequence, None),
        Err(error) => (
            SequenceDeclaration::default(),
            Some(DeferredSQLError::from(&error)),
        ),
    };
    Ok(AlterTableAction::SetIdentity {
        name: command.name.clone(),
        kind,
        repeated_kind,
        sequence,
        error,
    })
}

/// `DROP IDENTITY [ IF EXISTS ]`.
pub(super) fn drop_identity(command: &AlterTableCmd) -> AlterTableAction {
    AlterTableAction::DropIdentity {
        name: command.name.clone(),
        if_exists: command.missing_ok,
    }
}

fn identity_kind(generation: &str) -> Result<AutoIncrementKind, SQLError> {
    match generation {
        "a" => Ok(AutoIncrementKind::IdentityAlways),
        "d" => Ok(AutoIncrementKind::IdentityByDefault),
        other => Err(SQLError::Internal(format!(
            "identity has unknown generation {other:?}"
        ))),
    }
}
