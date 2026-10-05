//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove a table's own state; the catalog's dependencies decide what else a `DROP TABLE` removes.
pub mod context;
use context::TableRemovalContext;
use uqa_core::RelationIdentity;
use uqa_storage::{StorageBackendError, StorageBackendResult};
fn resolved_relation_identity(name: &str) -> StorageBackendResult<RelationIdentity> {
    RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)
}
fn table_not_found(table: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("table `{table}` does not exist"))
}
impl TableRemovalContext<'_> {
    pub fn hierarchy_drop_targets(
        &self,
        roots: &[String],
        cascade: bool,
    ) -> (Vec<String>, Vec<String>) {
        uqa_sql::schema::removal::hierarchy::hierarchy_drop_targets(self.hierarchy, roots, cascade)
    }
    /// Remove a table with its indexes, triggers and rules, the constraint triggers of other tables that reference it, and its storage; nothing else that depends on it.
    pub fn drop_table_state_inner(&self, name: &str) -> StorageBackendResult<()> {
        let relation = resolved_relation_identity(name)?;
        if !self.catalog.contains_relation(&relation) {
            return Err(table_not_found(name));
        }
        self.events.drop_relation_events_inner(&relation)?;
        crate::schema::indexes::diskann::retire_table(&self.indexes, name)?;
        self.publication.remove_state(name, &relation)
    }
}
