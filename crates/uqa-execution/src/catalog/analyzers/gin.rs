//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! GIN analyzer ownership follows complete physical input, native key syntax and ordered field publication.

use super::fields::{
    installation::{validate_declared_field, AnalyzerTableDefinition},
    GinAnalyzerOwners,
};
use std::collections::BTreeMap;
use uqa_sql::ast::IndexKey;
use uqa_storage::{
    CatalogFacade, CatalogIndexRow, RelationIdentity, StorageBackendError, StorageBackendResult,
};

/// The table owner publishes an absent physical FTS field at the original per-column boundary.
pub trait GinAnalyzerTable: AnalyzerTableDefinition {
    fn register_field(&self, field: &str) -> StorageBackendResult<()>;
}

/// State adapters preserve ordinary table resolution without exposing the enclosing Engine.
pub trait GinAnalyzerTables {
    type Table: GinAnalyzerTable;
    fn table(&self, name: &str) -> StorageBackendResult<Option<Self::Table>>;
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    tables: &impl GinAnalyzerTables,
) -> StorageBackendResult<GinAnalyzerOwners> {
    restore_rows(catalog.load_catalog_indexes()?, tables)
}

fn restore_rows(
    rows: Vec<CatalogIndexRow>,
    tables: &impl GinAnalyzerTables,
) -> StorageBackendResult<GinAnalyzerOwners> {
    let mut owners = GinAnalyzerOwners::new();
    for row in rows {
        if !row.index_type.eq_ignore_ascii_case("gin") {
            continue;
        }
        let name = RelationIdentity::from_legacy_name(&row.table_name)
            .map(|relation| relation.qualified_name())
            .map_err(corrupt)?;
        let table = tables
            .table(&name)?
            .ok_or_else(|| corrupt("GIN analyzer owner references a missing table"))?;
        let keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)?;
        let parameters: BTreeMap<String, String> = serde_json::from_str(&row.parameters_json)?;
        let analyzer = parameters
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("analyzer"))
            .map(|(_, name)| name.trim());
        for field in keys.iter().filter_map(IndexKey::column) {
            if !table.fields().iter().any(|current| current == field) {
                table.register_field(field)?;
            }
            if table.columns_declared() {
                validate_declared_field(&table, &name, field)?;
            }
            if let Some(analyzer) = analyzer {
                if owners
                    .insert((name.clone(), field.to_owned()), analyzer.to_owned())
                    .is_some_and(|previous| previous != analyzer)
                {
                    return Err(corrupt("GIN definitions have competing analyzer owners"));
                }
            }
        }
    }
    Ok(owners)
}

fn corrupt(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
