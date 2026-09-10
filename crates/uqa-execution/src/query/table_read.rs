//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only access retained by physical table sources.

use parking_lot::RwLockReadGuard;
use uqa_sql::ast::ColumnDef;
use uqa_storage::document_store::DocumentStore;

/// Only column definitions and shared document reads are exposed. A scan cannot mutate catalog state, obtain an engine, or acquire a storage write guard.
pub trait TableRead: Send + Sync {
    fn column_definitions(&self) -> Vec<ColumnDef>;
    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>>;
}

/// The chosen transaction snapshot supplies a table generation and the command's row overlay separately.
pub trait QueryTableAccess: Sync {
    fn table(&self, name: &str) -> Result<std::sync::Arc<dyn TableRead>, uqa_sql::SQLError>;
    fn command_overlay_changes(
        &self,
        name: &str,
    ) -> Result<
        Option<std::collections::BTreeMap<uqa_core::DocId, Option<uqa_storage::StoredDocument>>>,
        uqa_sql::SQLError,
    >;
    fn table_doc_count(&self, name: &str) -> Result<u64, uqa_sql::SQLError>;
}
