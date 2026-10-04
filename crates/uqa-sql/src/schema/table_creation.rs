//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE column validation and CREATE TABLE AS result declarations.
use crate::{
    ast::{ColumnDef, CreateTable},
    type_resolution::FunctionTypeResolver,
    ColumnType, SQLError,
};
use std::collections::BTreeSet;

/// `CheckAttributeNamesTypes`: no column may take a system column's name, and then no column may have a pseudo-type.
pub fn validate_create_table_columns(table: &CreateTable) -> Result<(), SQLError> {
    validate_relation_column_names_and_types(&table.columns)
}

fn validate_relation_column_names_and_types(columns: &[ColumnDef]) -> Result<(), SQLError> {
    for column in columns {
        super::columns::validate_postgres_column_name(&column.name)?;
    }
    for column in columns {
        super::columns::validate_postgres_relation_column_type(&column.name, &column.ty)?;
    }
    Ok(())
}

/// `DefineRelation` for a table whose columns a query defines: `MergeAttributes` rejects a repeated name, `BuildDescForRelation` requires `USAGE` on every column's type, and `CheckAttributeNamesTypes` rejects system column names and pseudo-types.
pub fn validate_create_table_as_columns(
    types: &dyn FunctionTypeResolver,
    columns: &[ColumnDef],
) -> Result<(), SQLError> {
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(&column.name) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{}\" specified more than once", column.name),
            });
        }
    }
    for column in columns {
        types.require_type_usage(&column.ty)?;
    }
    validate_relation_column_names_and_types(columns)
}

/// The columns of a table that a query defines, named by the column list and then by the query, as `intorel_startup` names them: more names than the query has columns are rejected.
pub fn create_table_as_columns(
    query_schema: &crate::RowSchema,
    column_names: &[String],
) -> Result<Vec<ColumnDef>, SQLError> {
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
    let columns = names
        .into_iter()
        .enumerate()
        .map(|(position, name)| ColumnDef {
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
            not_null_identity: None,
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
            check_catalog_oid: None,
            default_catalog_oid: None,
            references: None,
        })
        .collect::<Vec<_>>();
    Ok(columns)
}

pub mod checks;
pub mod declaration;
pub mod keys;
