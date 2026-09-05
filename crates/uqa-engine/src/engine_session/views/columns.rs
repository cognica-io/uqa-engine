//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic persistence of view column-reference changes.

use super::{
    catalog_view_row, query_plan_references_relation, Engine, RelationIdentity,
    StorageBackendError, StorageBackendResult,
};

impl Engine {
    pub(crate) fn rewrite_view_column_references(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let target =
            RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
        let mut next = self.durable.views.read().clone();
        let mut changed = Vec::new();
        for (relation, view) in &mut next {
            if query_plan_references_relation(
                &view.query,
                &target,
                &std::collections::BTreeSet::new(),
            ) {
                crate::sql::rename_view_column_query(self, &mut view.query, table, from, to)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                changed.push(relation.clone());
            }
        }
        if changed.is_empty() {
            return Ok(());
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            for relation in &changed {
                let view = next
                    .get(relation)
                    .expect("rewritten view retained in replacement catalog");
                if view.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                    catalog.save_view(&catalog_view_row(relation, view)?)?;
                }
            }
        }
        *self.durable.views.write() = next;
        self.note_catalog_registry_changed();
        Ok(())
    }
}
