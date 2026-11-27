//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A single retained source owns the population census, journal pruning and consuming rebuild.

use super::{invalid, DiskANNBoundRead, DiskANNPersistentBackend};
use crate::{
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
use std::sync::Arc;
use uqa_core::memory::{MemoryError, MemoryReservation};

pub struct PersistentDiskANNMaintenance<B: DiskANNPersistentBackend> {
    canonical: B,
    source: B::Read,
    coverage: KeyValueDiskANNPruner,
    resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl<B: DiskANNPersistentBackend> PersistentDiskANNMaintenance<B> {
    pub fn open(
        canonical: B,
        index: &RelationIdentity,
        resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
        max_record_bytes: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Box<Self>> {
        control.check()?;
        if canonical.in_transaction() {
            return Err(invalid("journal discovery requires an inactive session"));
        }
        let [table, field] = canonical.name_payload_bytes();
        let bytes = size_of::<Self>()
            .checked_add(table)
            .and_then(|bytes| bytes.checked_add(field))
            .ok_or(MemoryError::SizeOverflow)?;
        let memory = control.memory().reserve(bytes)?;
        let source = canonical.retain_for_index(index, control)?;
        let physical = source
            .selected_source(&*resolver, control)?
            .ok_or_else(|| invalid("journal discovery has no published generation"))?;
        let coverage = KeyValueDiskANNPruner::open(physical, max_record_bytes, control)?;
        Ok(Box::new(Self {
            canonical,
            source,
            coverage,
            resolver,
            control: control.clone(),
            _memory: memory,
        }))
    }
}

impl<B: DiskANNPersistentBackend> DiskANNJournalPruner for PersistentDiskANNMaintenance<B> {
    fn prune(
        &self,
        request: DiskANNPruneRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNPruneResult> {
        control.check()?;
        if !self.canonical.in_transaction() {
            return Err(invalid("journal page requires the caller's transaction"));
        }
        self.canonical.prune_page(
            &self.source,
            &*self.resolver,
            &self.coverage,
            request,
            control,
        )
    }
}

impl<B: DiskANNPersistentBackend> DiskANNMaintenanceSource for PersistentDiskANNMaintenance<B> {
    fn statistics(
        &self,
        request: DiskANNStatisticsRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNStatisticsPage> {
        self.check_invocation(control)?;
        B::measure_changes(&self.source, &self.coverage, request, control)
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

impl<B: DiskANNPersistentBackend> PersistentDiskANNMaintenance<B> {
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
