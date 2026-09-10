//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub use uqa_sql::catalog::index::IndexDefinition;
use uqa_storage::{CatalogIndexRow, StorageBackendError, StorageBackendResult};

pub fn index_definition(index: &CatalogIndexRow) -> StorageBackendResult<IndexDefinition> {
    uqa_sql::catalog::index::stored::index_definition(index.definition_json.as_deref())
        .map_err(StorageBackendError::from)
}

pub fn index_references_column(
    index: &CatalogIndexRow,
    column: &str,
) -> StorageBackendResult<bool> {
    uqa_sql::catalog::index::stored::references_column(
        &index.columns_json,
        index.definition_json.as_deref(),
        column,
    )
    .map_err(StorageBackendError::from)
}
