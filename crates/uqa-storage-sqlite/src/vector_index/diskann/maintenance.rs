//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::SQLiteDiskANNCanonical;
use std::sync::Arc;
use uqa_storage::{
    diskann_index::{
        catalog::DiskANNIndexResolver, changes::DiskANNJournalPruner,
        maintenance::DiskANNMaintenanceSource, PersistentDiskANNMaintenance,
    },
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendResult,
};

impl SQLiteDiskANNCanonical {
    pub fn journal_pruner(
        self,
        index: &RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        max_record_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Box<dyn DiskANNJournalPruner>> {
        Ok(self.maintenance_source(index, resolver, max_record_bytes, control)?)
    }

    pub fn maintenance_source(
        self,
        index: &RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        max_record_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Box<dyn DiskANNMaintenanceSource>> {
        Ok(PersistentDiskANNMaintenance::open(
            self,
            index,
            resolver,
            max_record_bytes,
            control,
        )?)
    }
}
