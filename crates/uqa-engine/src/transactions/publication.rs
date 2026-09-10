//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publication of durable transaction changes after the backend commit.

use std::sync::atomic::Ordering;

use super::Engine;

impl Engine {
    pub(super) fn publish_committed_transaction_epochs(&self) {
        if self.epochs.table_catalog.dirty.load(Ordering::Acquire) {
            self.publish_table_catalog_changes();
        }
        if self.epochs.catalog_registry.dirty.load(Ordering::Acquire) {
            self.publish_catalog_registry_changes();
        }
        if self.epochs.table_data.dirty.load(Ordering::Acquire) {
            self.publish_table_data_changes();
        }
    }
}
