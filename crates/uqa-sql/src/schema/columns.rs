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

pub fn reject_default_change_on_generated_column(
    catalog: &dyn crate::assignment::columns::AssignmentColumnCatalog,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    if crate::assignment::columns::generated_column_kind(catalog, table, column)?.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "column `{column}` of relation `{table}` is a generated column; use SET EXPRESSION or DROP EXPRESSION"
        )));
    }
    Ok(())
}

pub mod addition;
