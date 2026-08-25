//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared CREATE/ALTER validation for table foreign keys.

use super::{ColumnType, Engine, SQLError};
use uqa_sql::ast::{ColumnDef, ForeignKey, TableKeyConstraint};

pub(super) fn validate_foreign_key_definition(
    local_table: &str,
    local_columns: &[ColumnDef],
    parent_table: &str,
    parent_columns: &[ColumnDef],
    parent_keys: &[TableKeyConstraint],
    foreign_key: &ForeignKey,
) -> Result<(), SQLError> {
    if foreign_key.local_columns.is_empty()
        || foreign_key.local_columns.len() != foreign_key.ref_columns.len()
    {
        return Err(invalid_foreign_key(format!(
            "foreign key on relation \"{local_table}\" has mismatched local and referenced columns"
        )));
    }

    let local_types = foreign_key
        .local_columns
        .iter()
        .map(|name| {
            local_columns
                .iter()
                .find(|column| column.name == *name)
                .map(|column| &column.ty)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{local_table}.{name}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let parent_types = foreign_key
        .ref_columns
        .iter()
        .map(|name| {
            parent_columns
                .iter()
                .find(|column| column.name == *name)
                .map(|column| &column.ty)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{parent_table}.{name}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let target_key = parent_keys.iter().find(|constraint| {
        constraint.columns == foreign_key.ref_columns && constraint.without_overlaps
    });
    if foreign_key.period && target_key.is_none() {
        return Err(invalid_foreign_key(format!(
            "there is no primary key or unique constraint declared WITH WITHOUT OVERLAPS matching the referenced columns for table \"{parent_table}\""
        )));
    }

    if foreign_key.period {
        if foreign_key.local_columns.len() < 2 {
            return Err(invalid_foreign_key(
                "PERIOD foreign key must contain at least one ordinary column and one period column",
            ));
        }
        let local_period = local_types.last().expect("non-empty foreign key");
        let parent_period = parent_types.last().expect("non-empty foreign key");
        if !matches!(
            local_period,
            ColumnType::Range(_) | ColumnType::Multirange(_)
        ) || local_period != parent_period
        {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "PERIOD columns \"{}\" and \"{}\" have incompatible types {} and {}",
                    foreign_key
                        .local_columns
                        .last()
                        .expect("non-empty foreign key"),
                    foreign_key
                        .ref_columns
                        .last()
                        .expect("non-empty foreign key"),
                    local_period.sql_name(),
                    parent_period.sql_name()
                ),
            });
        }
    }

    Ok(())
}

pub(super) fn resolve_foreign_key_parent(
    engine: &Engine,
    reference: &str,
) -> Result<(String, Vec<ColumnDef>, Vec<TableKeyConstraint>), SQLError> {
    let canonical = engine
        .try_resolve_table_name(reference)
        .map_err(|error| SQLError::Internal(format!("resolve FOREIGN KEY target: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(reference.to_string()))?;
    let columns = engine
        .try_describe_table(&canonical)
        .map_err(|error| SQLError::Internal(format!("describe FOREIGN KEY target: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(canonical.clone()))?;
    let keys = engine
        .try_key_constraints(&canonical)
        .map_err(|error| SQLError::Internal(format!("read FOREIGN KEY target keys: {error}")))?;
    Ok((canonical, columns, keys))
}

fn invalid_foreign_key(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42830".into(),
        message: message.into(),
    }
}
