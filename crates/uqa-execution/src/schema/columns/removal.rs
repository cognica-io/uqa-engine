//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The inputs `ALTER TABLE DROP COLUMN` removes columns with, and the view lookup the column primitive checks its preconditions with.
use uqa_storage::StorageBackendResult;

pub trait ColumnRemovalViews {
    fn dependents(&self, table: &str, column: &str) -> StorageBackendResult<Vec<String>>;
}
pub struct ColumnRemovalContext<'a> {
    pub deletion: &'a dyn crate::schema::deletion::CatalogRemovalInputs,
}
