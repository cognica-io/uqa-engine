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
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

mod input;
pub use input::{GinAnalyzerCatalogInput, GinAnalyzerInputTable, GinAnalyzerInputTables};
mod tables;
pub use tables::lookup_table_from;
mod ordinary;
mod restoration;
use ordinary::restore_rows;

mod parameters;
pub use parameters::GinAnalyzerParameter;
mod owners;
pub use owners::{GinAnalyzerLookup, RetainedGinAnalyzerOwners};

/// The table owner publishes an absent physical FTS field at the original per-column boundary.
pub trait GinAnalyzerTable: AnalyzerTableDefinition {
    fn register_field(&self, field: &str) -> StorageBackendResult<()>;
}

/// State adapters preserve ordinary table resolution without exposing the enclosing Engine.
pub trait GinAnalyzerTables: Sync {
    type Table: GinAnalyzerTable;
    fn table(&self, name: &str) -> StorageBackendResult<Option<Self::Table>>;
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    tables: &impl GinAnalyzerTables,
) -> StorageBackendResult<GinAnalyzerOwners> {
    restore_rows(catalog.load_catalog_indexes()?, tables)
}

fn corrupt(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
