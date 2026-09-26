//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite journal discovery and current deletion evidence share the bound session.

use super::{invalid, KeyValueDiskANNCanonical, RetainedDiskANNCanonical};
use crate::diskann_index::{
    catalog::DiskANNIndexResolver,
    changes::{DiskANNJournalPruner, DiskANNPruneRequest, DiskANNPruneResult},
};
use crate::key_value::KeyValueDiskANNPruner;
use crate::{
    read_control::StorageReadControl, KeyValueStore, RelationIdentity, StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::memory::MemoryReservation;

struct Pruner {
    store: Arc<dyn KeyValueStore>,
    source: RetainedDiskANNCanonical,
    coverage: KeyValueDiskANNPruner,
    resolver: Arc<dyn DiskANNIndexResolver + Send + Sync>,
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
        control.check()?;
        if self.index.store.in_transaction() {
            return Err(invalid("journal discovery requires an inactive session"));
        }
        let memory = control.memory().reserve(size_of::<Pruner>())?;
        let source = self.retain_for_index(index, control)?;
        let physical = source
            .selected_source(&*resolver, control)?
            .ok_or_else(|| invalid("journal discovery has no published generation"))?;
        let coverage = KeyValueDiskANNPruner::open(physical, max_record_bytes, control)?;
        Ok(Box::new(Pruner {
            store: self.index.store,
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
        if !self.store.in_transaction() {
            return Err(invalid("journal page requires the caller's transaction"));
        }
        let mut result = None;
        self.store.with_mutation(&mut |read, batch| {
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
