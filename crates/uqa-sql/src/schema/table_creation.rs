//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE column validation and CREATE TABLE AS result declarations.
use crate::{ast::CreateTable, ColumnType, SQLError};
use std::collections::BTreeSet;

pub fn validate_create_table_columns(table: &CreateTable) -> Result<(), SQLError> {
    for column in &table.columns {
        super::columns::validate_postgres_column_name(&column.name)?;
        super::columns::validate_postgres_relation_column_type(&column.name, &column.ty)?;
    }
    Ok(())
}

pub fn create_table_as_columns(
    query_schema: &crate::RowSchema,
    column_names: &[String],
) -> Result<Vec<crate::ast::ColumnDef>, SQLError> {
    if column_names.len() > query_schema.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "too many column names were specified".into(),
        });
    }
    let names = query_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, name)| {
            column_names
                .get(position)
                .cloned()
                .unwrap_or_else(|| name.clone())
        })
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    for name in &names {
        super::columns::validate_postgres_column_name(name)?;
        if !seen.insert(name) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{name}\" specified more than once"),
            });
        }
    }
    let columns = names
        .into_iter()
        .enumerate()
        .map(|(position, name)| crate::ast::ColumnDef {
            name,
            ty: query_schema
                .column_type(position)
                .cloned()
                .unwrap_or(ColumnType::Text),
            object_id: None,
            missing_value: None,
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            not_null_validated: true,
            not_null_no_inherit: false,
            not_null_is_local: true,
            auto_increment: None,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            check_validated: true,
            check_no_inherit: false,
            check_is_local: true,
            check_object_id: None,
            references: None,
        })
        .collect::<Vec<_>>();
    for column in &columns {
        super::columns::validate_postgres_relation_column_type(&column.name, &column.ty)?;
    }
    Ok(columns)
}

pub mod declaration;
