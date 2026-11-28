//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table catalog readers retain existing metadata owners under their caller's catalog synchronization.

use crate::TableState;
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_execution::catalog::CatalogTableSnapshot;

impl TableState {
    pub(crate) fn catalog_snapshot_entry<'a>(
        (relation, table): (&'a RelationIdentity, &Arc<Self>),
    ) -> (&'a RelationIdentity, CatalogTableSnapshot) {
        (relation, table.catalog_snapshot())
    }

    pub(super) fn track_catalog_view_revision(&self, revision: &Arc<std::sync::atomic::AtomicU64>) {
        self.dropped_attributes.track_view_revision(revision);
        self.row_type_array_name.track_view_revision(revision);
        self.security.track_view_revision(revision);
        self.columns.track_view_revision(revision);
        self.columns_declared.track_view_revision(revision);
        self.table_checks.track_view_revision(revision);
        self.foreign_keys.track_view_revision(revision);
        self.key_constraints.track_view_revision(revision);
        self.hierarchy.track_view_revision(revision);
    }

    pub(crate) fn catalog_snapshot(&self) -> CatalogTableSnapshot {
        CatalogTableSnapshot {
            dropped_attributes: self.dropped_attributes.snapshot(),
            object_id: self.object_id(),
            catalog_oids: self.relation_oids(),
            row_type_array_name: self.row_type_array_name.snapshot(),
            security: self.security.snapshot(),
            columns: self.columns.snapshot(),
            columns_declared: *self.columns_declared.read(),
            checks: self.table_checks.snapshot(),
            foreign_keys: self.foreign_keys.snapshot(),
            keys: self.key_constraints.snapshot(),
            hierarchy: self.hierarchy.snapshot(),
            persistence: self.persistence,
        }
    }
}

#[cfg(test)]
mod tests;
