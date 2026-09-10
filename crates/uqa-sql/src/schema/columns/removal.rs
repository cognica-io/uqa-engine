//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Discover foreign keys that refer to a deleted column in catalog order.
use crate::{assignment::columns::ColumnCatalogError, ast::ForeignKey, SQLError};
pub trait ColumnRemovalCatalog {
    fn try_resolve_table_name(&self, table: &str) -> Result<Option<String>, ColumnCatalogError>;
    fn table_names(&self) -> Result<Vec<String>, ColumnCatalogError>;
    fn try_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, ColumnCatalogError>;
}
pub fn foreign_keys_referencing_column(
    catalog: &dyn ColumnRemovalCatalog,
    table: &str,
    column: &str,
) -> Result<Vec<(String, String)>, SQLError> {
    let canonical = catalog
        .try_resolve_table_name(table)
        .map_err(|error| {
            crate::catalog::errors::storage_error("ALTER TABLE DROP COLUMN", error.as_ref())
        })?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut dependents = Vec::new();
    for referrer in catalog.table_names().map_err(|error| {
        crate::catalog::errors::storage_error("ALTER TABLE DROP COLUMN", error.as_ref())
    })? {
        for foreign_key in catalog.try_foreign_keys(&referrer).map_err(|error| {
            crate::catalog::errors::storage_error("ALTER TABLE DROP COLUMN", error.as_ref())
        })? {
            if foreign_key.ref_table == canonical
                && foreign_key.ref_columns.iter().any(|name| name == column)
            {
                dependents.push((
                    referrer.clone(),
                    foreign_key.name.clone().ok_or_else(|| {
                        SQLError::Internal("dependent FOREIGN KEY has no durable name".into())
                    })?,
                ));
            }
        }
    }
    Ok(dependents)
}
