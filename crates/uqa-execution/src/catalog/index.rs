//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub use uqa_sql::catalog::index::IndexDefinition;
use uqa_storage::{CatalogIndexRow, StorageBackendError, StorageBackendResult};

pub fn index_definition(index: &CatalogIndexRow) -> StorageBackendResult<IndexDefinition> {
    index.definition_json.as_deref().map_or_else(
        || Ok(IndexDefinition::default()),
        |definition| serde_json::from_str(definition).map_err(StorageBackendError::from),
    )
}
