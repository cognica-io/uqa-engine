//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared and hidden column schemas shared by mutation analysis and row execution.
use super::{DOC_ID_COLUMN, TABLE_OID_COLUMN, XMIN_COLUMN};
use crate::{ast::ColumnDef, ColumnType, RowSchema, SQLError};

/// Relation metadata observed by mutation row construction.
pub trait MutationRowCatalog: Sync {
    fn column_definitions(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String>;
    fn column_names(&self, table: &str) -> Result<Vec<String>, String>;
    fn view_schema(&self, name: &str) -> Result<RowSchema, SQLError>;
}

pub fn null_target_schema(
    relations: &dyn MutationRowCatalog,
    table: &str,
    qualifier: &str,
) -> Result<RowSchema, SQLError> {
    let definitions = relations
        .column_definitions(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut columns = if definitions.is_empty() {
        relations
            .column_names(table)
            .map_err(|error| dml_storage_error("DML row schema lookup", error))?
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(ColumnType::Oid));
    columns.push(XMIN_COLUMN.into());
    types.push(Some(ColumnType::Xid));
    Ok(RowSchema::with_qualified_types(qualifier, columns, types))
}

fn dml_storage_error(action: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {error}"))
}
