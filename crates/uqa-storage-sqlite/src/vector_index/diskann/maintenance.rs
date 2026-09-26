//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native journal pages retain finite discovery separately from current deletion evidence.

use super::{invalid, RetainedSQLiteDiskANNCanonical, SQLiteDiskANNCanonical};
use crate::ManagedConnection;
use std::sync::Arc;
use uqa_core::memory::MemoryReservation;
use uqa_storage::{
    diskann_index::{
        catalog::DiskANNIndexResolver,
        changes::{DiskANNJournalPruner, DiskANNPruneRequest, DiskANNPruneResult},
    },
    key_value::KeyValueDiskANNPruner,
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendResult,
};

struct Pruner {
    connection: ManagedConnection,
    source: RetainedSQLiteDiskANNCanonical,
    coverage: KeyValueDiskANNPruner,
    resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
    _memory: MemoryReservation,
}

impl SQLiteDiskANNCanonical {
    pub fn journal_pruner(
        self,
        index: &RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        max_record_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Box<dyn DiskANNJournalPruner>> {
        control.check()?;
        if self.index.conn.in_transaction() {
            return Err(invalid("journal discovery requires an inactive session"));
        }
        let memory = control.memory().reserve(size_of::<Pruner>())?;
        let source = self.retain_for_index(index, control)?;
        let physical = source
            .selected_source(&*resolver, control)?
            .ok_or_else(|| invalid("journal discovery has no published generation"))?;
        let coverage = KeyValueDiskANNPruner::open(physical, max_record_bytes, control)?;
        Ok(Box::new(Pruner {
            connection: self.index.conn,
            source,
            coverage,
            resolver,
            _memory: memory,
        }))
    }
}

impl DiskANNJournalPruner for Pruner {
    fn prune(
        &self,
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        control.check()?;
        if !self.connection.in_transaction() {
            return Err(invalid("journal page requires the caller's transaction"));
        }
        self.connection
            .with_native_write(|current, batch| {
                Ok(self.source.prune_captured_changes(
                    &*self.resolver,
                    &self.coverage,
                    (current, batch),
                    request,
                    control,
                )?)
            })?
            .ok_or_else(|| invalid("journal page requires a bound native session"))
    }
}
