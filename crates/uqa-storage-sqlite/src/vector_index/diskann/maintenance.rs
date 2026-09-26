//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native maintenance retains one canonical capture for change accounting, pruning discovery and a consuming rebuild.

use super::{invalid, RetainedSQLiteDiskANNCanonical, SQLiteDiskANNCanonical};
use std::sync::Arc;
use uqa_core::memory::MemoryReservation;
use uqa_storage::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        catalog::DiskANNIndexResolver,
        changes::{DiskANNJournalPruner, DiskANNPruneRequest, DiskANNPruneResult},
        maintenance::{DiskANNMaintenanceSource, DiskANNStatisticsPage, DiskANNStatisticsRequest},
        DiskANNIndexOptions,
    },
    key_value::KeyValueDiskANNPruner,
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendResult,
};

struct MaintenanceSource {
    canonical: SQLiteDiskANNCanonical,
    source: RetainedSQLiteDiskANNCanonical,
    coverage: KeyValueDiskANNPruner,
    resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
    control: StorageReadControl,
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
        Ok(self.maintenance_source(index, resolver, max_record_bytes, control)?)
    }

    pub fn maintenance_source(
        self,
        index: &RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        max_record_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Box<dyn DiskANNMaintenanceSource>> {
        control.check()?;
        if self.index.conn.in_transaction() {
            return Err(invalid("journal discovery requires an inactive session"));
        }
        let bytes = size_of::<MaintenanceSource>()
            .checked_add(self.index.table.capacity())
            .and_then(|bytes| bytes.checked_add(self.index.field.capacity()))
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let memory = control.memory().reserve(bytes)?;
        let source = self.retain_for_index(index, control)?;
        let physical = source
            .selected_source(&*resolver, control)?
            .ok_or_else(|| invalid("journal discovery has no published generation"))?;
        let coverage = KeyValueDiskANNPruner::open(physical, max_record_bytes, control)?;
        Ok(Box::new(MaintenanceSource {
            canonical: self,
            source,
            coverage,
            resolver,
            control: control.clone(),
            _memory: memory,
        }))
    }
}

impl DiskANNJournalPruner for MaintenanceSource {
    fn prune(
        &self,
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        control.check()?;
        let connection = &self.canonical.index.conn;
        if !connection.in_transaction() {
            return Err(invalid("journal page requires the caller's transaction"));
        }
        connection
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

impl DiskANNMaintenanceSource for MaintenanceSource {
    fn statistics(
        &self,
        request: DiskANNStatisticsRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNStatisticsPage> {
        self.check_invocation(control)?;
        self.source
            .measure_changes(&self.coverage, request, control)
    }

    fn rebuild(
        self: Box<Self>,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.check_invocation(control)?;
        let Self {
            canonical,
            source,
            coverage,
            resolver,
            control: _,
            _memory,
        } = *self;
        drop(coverage);
        canonical.rebuild_source(source, &*resolver, options, temporary, control)
    }
}

impl MaintenanceSource {
    fn check_invocation(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        control.check()?;
        if !self.control.memory().shares_allowance(control.memory())
            || !self
                .control
                .cancellation()
                .shares_signal(control.cancellation())
        {
            return Err(invalid(
                "maintenance requires its original allowance and cancellation",
            ));
        }
        Ok(())
    }
}
