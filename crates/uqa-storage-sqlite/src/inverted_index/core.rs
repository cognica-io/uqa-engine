//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construction, analyzer selection, and physical auxiliary names.

use super::{
    Analyzer, InvertedIndex, ManagedConnection, SQLiteInvertedIndex, StorageBackendResult,
    DEFAULT_BLOCK_SIZE,
};

impl SQLiteInvertedIndex {
    pub const BLOCK_SIZE: usize = DEFAULT_BLOCK_SIZE;

    pub fn new(conn: ManagedConnection, table: impl Into<String>, analyzer: Analyzer) -> Self {
        Self {
            conn,
            table: table.into(),
            bindings: uqa_storage::inverted_index::AnalyzerBindings::new(analyzer),
        }
    }

    /// Tokenize `text` with the analyzer bound to `field`.
    pub fn tokenize(&self, text: &str, field: &str) -> StorageBackendResult<Vec<String>> {
        Ok(self.bindings.index_revision(field)?.analyze(text)?)
    }

    pub fn skip_table_name(&self, field: &str) -> String {
        format!("_skip_{}_{}", self.table, field)
    }

    pub fn blockmax_table_name(&self, field: &str) -> String {
        format!("_blockmax_{}_{}", self.table, field)
    }

    pub fn flush_skip_pointers(&self) -> StorageBackendResult<()> {
        let fields = self.field_names()?;
        for field in fields {
            self.rebuild_skip_pointers_for_field(&field)?;
        }
        Ok(())
    }
}
