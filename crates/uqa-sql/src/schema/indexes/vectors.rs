//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL vector-index target identity, key type, and existing-index checks.
use crate::{
    ast::{ColumnType, CreateIndex},
    SQLError,
};
pub trait VectorIndexCatalog {
    fn resolve_table_name(&self, name: &str) -> Result<Option<String>, SQLError>;
    fn column_type(&self, table: &str, column: &str) -> Result<Option<ColumnType>, SQLError>;
    fn vector_index_names(&self, table: &str, column: &str) -> Result<Vec<String>, SQLError>;
}
pub struct VectorIndexTarget<'a> {
    pub table: String,
    pub fields: Vec<(&'a str, u32)>,
}
pub fn resolve_vector_index_target<'a>(
    catalog: &dyn VectorIndexCatalog,
    statement: &'a CreateIndex,
    access_method: &str,
) -> Result<VectorIndexTarget<'a>, SQLError> {
    let table = catalog
        .resolve_table_name(&statement.table)?
        .ok_or_else(|| {
            SQLError::Unsupported(format!(
                "CREATE INDEX USING {access_method}: relation `{}` does not exist",
                statement.table
            ))
        })?;
    let mut fields = Vec::with_capacity(statement.columns.len());
    for key in &statement.columns {
        let column = super::keys::require_column_key(key, access_method)?;
        let dimensions = match catalog.column_type(&table, column)? {
            Some(ColumnType::Vector(dim) | ColumnType::Tensor(dim)) => dim,
            Some(other) => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE INDEX USING {access_method} requires VECTOR or TENSOR column `{column}`, got {other:?}"
                )));
            }
            None => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE INDEX USING {access_method}: column `{table}`.`{column}` does not exist"
                )));
            }
        };
        let existing = catalog.vector_index_names(&table, column)?;
        if !existing.is_empty() {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING {access_method}: `{table}`.`{column}` already has physical vector index `{}`",
                existing.join("`, `")
            )));
        }
        fields.push((column, dimensions));
    }
    Ok(VectorIndexTarget { table, fields })
}
