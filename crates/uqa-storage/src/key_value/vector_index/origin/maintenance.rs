//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One canonical capture supplies exact change accounting, finite pruning discovery and a consuming rebuild.

use super::{invalid, KeyValueDiskANNCanonical, RetainedDiskANNCanonical};
use crate::diskann_index::{
    build::DiskANNTemporaryBudget,
    catalog::DiskANNIndexResolver,
    changes::{DiskANNJournalPruner, DiskANNPruneRequest, DiskANNPruneResult},
    maintenance::{DiskANNMaintenanceSource, DiskANNStatisticsPage, DiskANNStatisticsRequest},
    DiskANNIndexOptions,
};
use crate::key_value::KeyValueDiskANNPruner;
use crate::{read_control::StorageReadControl, RelationIdentity, StorageBackendResult};
use std::sync::Arc;
use uqa_core::memory::MemoryReservation;

struct MaintenanceSource {
    canonical: KeyValueDiskANNCanonical,
    source: RetainedDiskANNCanonical,
    coverage: KeyValueDiskANNPruner,
    resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl KeyValueDiskANNCanonical {
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
        if self.index.store.in_transaction() {
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
        let store = &self.canonical.index.store;
        if !store.in_transaction() {
            return Err(invalid("journal page requires the caller's transaction"));
        }
        let mut result = None;
        store.with_mutation(&mut |read, batch| {
            result = Some(self.source.prune_captured_changes(
                &*self.resolver,
                &self.coverage,
                (read, batch),
                request,
                control,
            )?);
            Ok(())
        })?;
        result.ok_or_else(|| invalid("journal page was not evaluated"))
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
        if !self.control.shares_context(control) {
            return Err(invalid(
                "maintenance requires its original allowance and cancellation",
            ));
        }
        Ok(())
    }
}
