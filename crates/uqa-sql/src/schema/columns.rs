//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` column-name and stored-type declaration rules.

use crate::{ColumnType, SQLError};

pub const POSTGRES_SYSTEM_COLUMNS: [&str; 6] = ["tableoid", "xmin", "cmin", "xmax", "cmax", "ctid"];

pub fn validate_postgres_column_name(name: &str) -> Result<(), SQLError> {
    if POSTGRES_SYSTEM_COLUMNS.contains(&name) {
        return Err(SQLError::Routine {
            sqlstate: "42701".into(),
            message: format!("column name \"{name}\" conflicts with a system column name"),
        });
    }
    Ok(())
}

pub fn validate_postgres_relation_column_type(name: &str, ty: &ColumnType) -> Result<(), SQLError> {
    let pseudo_type = match ty {
        ColumnType::Void | ColumnType::AnyArray | ColumnType::Record => Some(ty.sql_name()),
        ColumnType::Array(element)
            if matches!(
                element.as_ref(),
                ColumnType::Void | ColumnType::AnyArray | ColumnType::Record
            ) =>
        {
            Some(ty.sql_name())
        }
        _ => None,
    };
    if let Some(pseudo_type) = pseudo_type {
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: format!("column \"{name}\" has pseudo-type {pseudo_type}"),
        });
    }
    Ok(())
}

/// Append a declared column after checking its name against the existing schema.
pub fn append_registered_column(
    table: &str,
    columns: &mut Vec<crate::ast::ColumnDef>,
    column: crate::ast::ColumnDef,
) -> Result<(), String> {
    if columns.iter().any(|existing| existing.name == column.name) {
        return Err(format!(
            "column `{}` already exists on table `{table}`",
            column.name
        ));
    }
    columns.push(column);
    Ok(())
}

/// Reject `SET DEFAULT`, when `setting`, or `DROP DEFAULT` on an identity or generated column, as `PostgreSQL`'s `ATExecColumnDefault` does: its hint names the command that changes such a column.
pub fn reject_default_change(
    catalog: &dyn crate::assignment::columns::AssignmentColumnCatalog,
    table: &str,
    column: &str,
    setting: bool,
) -> Result<(), SQLError> {
    let Some(shape) = catalog
        .try_column_shape(table, column)
        .map_err(|error| SQLError::Internal(format!("read column `{column}`: {error}")))?
        .flatten()
    else {
        return Ok(());
    };
    let relation = uqa_core::RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve ALTER TABLE target: {error}")))?
        .name;
    let (kind, hint) = if shape.identity_sequence.is_some() {
        (
            "an identity column",
            (!setting).then_some("ALTER TABLE ... ALTER COLUMN ... DROP IDENTITY"),
        )
    } else if let Some(generated) = shape.generated {
        (
            "a generated column",
            if setting {
                Some("ALTER TABLE ... ALTER COLUMN ... SET EXPRESSION")
            } else {
                (generated == crate::ast::GeneratedColumnKind::Stored)
                    .then_some("ALTER TABLE ... ALTER COLUMN ... DROP EXPRESSION")
            },
        )
    } else {
        return Ok(());
    };
    Err(SQLError::Diagnostic {
        sqlstate: "42601".into(),
        message: format!("column \"{column}\" of relation \"{relation}\" is {kind}"),
        detail: None,
        hint: hint.map(|command| format!("Use {command} instead.")),
    })
}

pub mod addition;

pub mod publication;

pub mod alteration;

pub mod removal;

pub mod removal_metadata;
