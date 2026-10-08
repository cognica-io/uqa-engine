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

/// `renameatt_internal`: find the original live attribute before checking the destination. Views and standalone composite relations have no system attributes.
pub fn renamed_column_position<'a>(
    relation: &str,
    columns: impl IntoIterator<Item = &'a str>,
    from: &str,
    to: &str,
    has_system_columns: bool,
) -> Result<usize, SQLError> {
    let mut position = None;
    let mut duplicate = false;
    for (index, column) in columns.into_iter().enumerate() {
        if column == from {
            position = Some(index);
        }
        duplicate |= column == to;
    }
    let position = position.ok_or_else(|| SQLError::Routine {
        sqlstate: "42703".into(),
        message: format!("column \"{from}\" does not exist"),
    })?;
    if has_system_columns {
        validate_postgres_column_name(to)?;
    }
    if duplicate {
        return Err(SQLError::Routine {
            sqlstate: "42701".into(),
            message: format!("column \"{to}\" of relation \"{relation}\" already exists"),
        });
    }
    Ok(position)
}

/// `column "x" of relation "t" does not exist`, which `ALTER TABLE` reports for a column the relation lacks.
pub fn undefined_relation_column(table: &str, column: &str) -> SQLError {
    match uqa_core::RelationIdentity::from_legacy_name(table) {
        Ok(relation) => SQLError::Routine {
            sqlstate: "42703".into(),
            message: format!(
                "column \"{column}\" of relation \"{}\" does not exist",
                relation.name
            ),
        },
        Err(error) => SQLError::Internal(format!("resolve ALTER TABLE target `{table}`: {error}")),
    }
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

/// Check a DROP target before dependency traversal. Missing `IF EXISTS` targets return false so execution can publish its notice without changing the relation.
pub fn validate_drop_column(
    columns: &[crate::ast::ColumnDef],
    table: &str,
    column: &str,
    if_exists: bool,
) -> Result<bool, SQLError> {
    if POSTGRES_SYSTEM_COLUMNS.contains(&column) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("cannot drop system column \"{column}\""),
        });
    }
    if columns.iter().any(|definition| definition.name == column) {
        return Ok(true);
    }
    if if_exists {
        Ok(false)
    } else {
        Err(undefined_relation_column(table, column))
    }
}

/// The notice for a missing column, using the unqualified name of the already bound relation.
pub fn missing_drop_column_notice(table: &str, column: &str) -> crate::SQLNotice {
    crate::SQLNotice::notice(format!(
        "column \"{column}\" of relation \"{table}\" does not exist, skipping"
    ))
}

/// The column an `ALTER TABLE ... ALTER COLUMN` action names, as `get_attnum` finds it for `ATExecColumnDefault` and its siblings.
pub fn altered_column<'a>(
    table: &str,
    columns: &'a [crate::ast::ColumnDef],
    column: &str,
) -> Result<&'a crate::ast::ColumnDef, SQLError> {
    columns
        .iter()
        .find(|definition| definition.name == column)
        .ok_or_else(|| missing_altered_column(table, column))
}

/// The error for an `ALTER COLUMN` target that is not a column of the relation: every relation has the system columns, which cannot be altered, and any other name does not exist.
pub fn missing_altered_column(table: &str, column: &str) -> SQLError {
    if POSTGRES_SYSTEM_COLUMNS.contains(&column) {
        return SQLError::Routine {
            sqlstate: "0A000".into(),
            message: format!("cannot alter system column \"{column}\""),
        };
    }
    undefined_relation_column(table, column)
}

/// `ATExecColumnDefault`'s checks of the column whose default `SET DEFAULT` (`setting`) or `DROP DEFAULT` changes: an identity column takes its values from its sequence and a generated column from its expression.
pub fn validate_default_change(
    table: &str,
    column: &crate::ast::ColumnDef,
    setting: bool,
) -> Result<(), SQLError> {
    let relation = uqa_core::RelationIdentity::from_legacy_name(table).map_err(|error| {
        SQLError::Internal(format!("resolve ALTER TABLE target `{table}`: {error}"))
    })?;
    if column
        .auto_increment
        .as_ref()
        .is_some_and(crate::ast::AutoIncrement::is_identity)
    {
        return Err(SQLError::Diagnostic {
            sqlstate: "42601".into(),
            message: format!(
                "column \"{}\" of relation \"{}\" is an identity column",
                column.name, relation.name
            ),
            detail: None,
            hint: (!setting)
                .then(|| "Use ALTER TABLE ... ALTER COLUMN ... DROP IDENTITY instead.".into()),
        });
    }
    if let Some(generated) = &column.generated {
        let hint = if setting {
            Some("Use ALTER TABLE ... ALTER COLUMN ... SET EXPRESSION instead.")
        } else if generated.kind == crate::ast::GeneratedColumnKind::Stored {
            Some("Use ALTER TABLE ... ALTER COLUMN ... DROP EXPRESSION instead.")
        } else {
            None
        };
        return Err(SQLError::Diagnostic {
            sqlstate: "42601".into(),
            message: format!(
                "column \"{}\" of relation \"{}\" is a generated column",
                column.name, relation.name
            ),
            detail: None,
            hint: hint.map(Into::into),
        });
    }
    Ok(())
}

pub mod addition;

pub mod publication;

pub mod alteration;

pub mod removal_metadata;

pub mod type_target;

pub mod type_transform;
