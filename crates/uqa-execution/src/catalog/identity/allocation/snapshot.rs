//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep one immutable definition view for a catalog allocator until its source changes.

use crate::catalog::{services::CatalogSnapshotSource, CatalogReadView};

#[derive(Default)]
pub(super) struct AllocationCatalog {
    selected: Option<(u64, CatalogReadView)>,
}

impl AllocationCatalog {
    pub(super) fn current(&mut self, source: &dyn CatalogSnapshotSource) -> CatalogReadView {
        loop {
            let Some(revision) = source.current_catalog_revision() else {
                self.selected = None;
                return source.current_catalog_snapshot();
            };
            if let Some((selected, catalog)) = &self.selected {
                if *selected == revision {
                    return catalog.clone();
                }
            }
            let catalog = source.current_catalog_snapshot();
            if source.current_catalog_revision() != Some(revision) {
                continue;
            }
            self.selected = Some((revision, catalog.clone()));
            return catalog;
        }
    }
}
