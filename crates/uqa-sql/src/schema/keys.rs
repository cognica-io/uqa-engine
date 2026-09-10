//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declaration and identity rules for newly added PRIMARY KEY and UNIQUE constraints.
use crate::{
    ast::{ColumnDef, ColumnType, ForeignKey, TableCheck, TableKeyConstraint},
    SQLError,
};
pub fn validate_added_key_columns(
    table: &str,
    constraint: &TableKeyConstraint,
    columns: &[ColumnDef],
) -> Result<(), SQLError> {
    let column_names: std::collections::BTreeSet<&str> =
        columns.iter().map(|column| column.name.as_str()).collect();
    for column in &constraint.columns {
        if !column_names.contains(column.as_str()) {
            return Err(SQLError::TypeMismatch(format!(
                "ALTER TABLE ADD CONSTRAINT references unknown column `{column}`"
            )));
        }
    }
    if constraint.without_overlaps {
        let period_column = constraint.columns.last().ok_or_else(|| {
            SQLError::TypeMismatch(
                "constraint using WITHOUT OVERLAPS needs at least two columns".into(),
            )
        })?;
        let period_type = columns
            .iter()
            .find(|column| column.name == *period_column)
            .map(|column| &column.ty)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{period_column}")))?;
        if !matches!(
            period_type,
            ColumnType::Range(_) | ColumnType::Multirange(_)
        ) {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "column \"{period_column}\" in WITHOUT OVERLAPS is not a range or multirange type"
                ),
            });
        }
        if constraint.columns.len() < 2 {
            return Err(SQLError::TypeMismatch(
                "constraint using WITHOUT OVERLAPS needs at least two columns".into(),
            ));
        }
    }

    Ok(())
}
pub fn validate_added_key_identity(
    table: &str,
    constraint: &TableKeyConstraint,
    existing_keys: &[TableKeyConstraint],
    checks: &[TableCheck],
    foreign_keys: &[ForeignKey],
) -> Result<(), SQLError> {
    if let Some(name) = constraint.name.as_deref() {
        let check_name_exists = checks
            .iter()
            .any(|existing| existing.name.as_deref() == Some(name));
        let foreign_name_exists = foreign_keys
            .iter()
            .any(|existing| existing.name.as_deref() == Some(name));
        let key_name_exists = existing_keys
            .iter()
            .any(|existing| existing.name.as_deref() == Some(name));
        if check_name_exists || foreign_name_exists || key_name_exists {
            return Err(SQLError::TypeMismatch(format!(
                "constraint `{name}` already exists on table `{table}`"
            )));
        }
    }
    if constraint.kind == crate::ast::TableKeyConstraintKind::PrimaryKey
        && existing_keys
            .iter()
            .any(|existing| existing.kind == crate::ast::TableKeyConstraintKind::PrimaryKey)
    {
        return Err(SQLError::TypeMismatch(format!(
            "multiple PRIMARY KEY constraints are not allowed on table `{table}`"
        )));
    }

    Ok(())
}

/// Apply the NOT NULL requirement of a primary key to the stored column candidate.
pub fn apply_primary_key_columns(
    table: &str,
    constraint: &TableKeyConstraint,
    columns: &mut [ColumnDef],
) -> Result<(), String> {
    if constraint.kind == crate::ast::TableKeyConstraintKind::PrimaryKey {
        for key_column in &constraint.columns {
            let column = columns
                .iter_mut()
                .find(|column| column.name == *key_column)
                .ok_or_else(|| {
                    format!("column `{key_column}` does not exist on table `{table}`")
                })?;
            column.not_null = true;
        }
    }
    Ok(())
}
