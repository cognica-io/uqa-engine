//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read relation and text-index metadata from the active engine catalog.

use crate::Engine;
use uqa_sql::{semantics::text_indexes::TextMatchCatalog, SQLError};

impl TextMatchCatalog for Engine {
    fn has_table(&self, table: &str) -> Result<bool, String> {
        self.try_query_has_table(table)
            .map_err(|error| error.to_string())
    }
    fn has_column(&self, table: &str, column: &str) -> Result<bool, String> {
        self.try_query_table_has_column(table, column)
            .map_err(|error| error.to_string())
    }
    fn column_names(&self, table: &str) -> Result<Vec<String>, String> {
        self.try_query_table_columns(table)
            .map_err(|error| error.to_string())
    }
    fn indexed_fields(&self, table: &str) -> Result<Vec<String>, SQLError> {
        self.fts_fields_for_table(table)
    }
}
