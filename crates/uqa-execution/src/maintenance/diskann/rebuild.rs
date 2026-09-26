//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rebuild admission retains the exact census source and its original temporary allowance.

use uqa_storage::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        format::DiskANNGeneration,
        maintenance::{
            DiskANNChangeStatistics, DiskANNMaintenanceSource, DiskANNStatisticsCursor,
            DiskANNStatisticsRequest,
        },
        DiskANNIndexOptions,
    },
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

/// Soft triggers for one index's current uncovered documents or logical raw vector bytes. Neither limit is a write quota or a physical-retention limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskANNRebuildPolicy {
    documents: u64,
    vector_bytes: u64,
}

impl Default for DiskANNRebuildPolicy {
    fn default() -> Self {
        Self {
            documents: 1024,
            vector_bytes: 64 << 20,
        }
    }
}

impl DiskANNRebuildPolicy {
    pub fn new(documents: u64, vector_bytes: u64) -> StorageBackendResult<Self> {
        if documents == 0 || vector_bytes == 0 {
            return Err(StorageBackendError::Other(
                "DiskANN rebuild thresholds must be positive".into(),
            ));
        }
        Ok(Self {
            documents,
            vector_bytes,
        })
    }

    pub fn documents(self) -> u64 {
        self.documents
    }
    pub fn vector_bytes(self) -> u64 {
        self.vector_bytes
    }

    pub(super) fn admits(self, changes: DiskANNChangeStatistics) -> bool {
        changes.documents >= self.documents || changes.vector_bytes >= self.vector_bytes
    }
}

/// Exact counts for the most recently completed index capture; later commits may already have changed its live corpus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskANNMaintenanceCensus {
    pub generation: DiskANNGeneration,
    pub changes: DiskANNChangeStatistics,
}

#[derive(Clone)]
pub(super) struct Configuration {
    pub policy: DiskANNRebuildPolicy,
    pub temporary: DiskANNTemporaryBudget,
}

pub(super) struct Capture {
    pub source: Box<dyn DiskANNMaintenanceSource>,
    pub options: DiskANNIndexOptions,
    pub configuration: Configuration,
    pub ready: bool,
    after: Option<DiskANNStatisticsCursor>,
    generation: Option<DiskANNGeneration>,
    total: DiskANNChangeStatistics,
}

impl Capture {
    pub fn new(
        source: Box<dyn DiskANNMaintenanceSource>,
        options: DiskANNIndexOptions,
        configuration: Configuration,
    ) -> Self {
        Self {
            source,
            options,
            configuration,
            ready: false,
            after: None,
            generation: None,
            total: DiskANNChangeStatistics::default(),
        }
    }

    pub fn step(
        &mut self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNMaintenanceCensus>> {
        let page = self.source.statistics(
            DiskANNStatisticsRequest {
                after: self.after,
                max_records: 64,
            },
            control,
        )?;
        if page.examined > 64
            || (page.next.is_some() && (page.examined == 0 || page.next == self.after))
            || self
                .generation
                .is_some_and(|generation| generation != page.generation)
        {
            return Err(StorageBackendError::Other(
                "invalid DiskANN maintenance census page".into(),
            ));
        }
        control.check()?;
        self.total = self.total.checked_add(page.outstanding)?;
        self.generation = Some(page.generation);
        self.after = page.next;
        Ok(page.next.is_none().then_some(DiskANNMaintenanceCensus {
            generation: page.generation,
            changes: self.total,
        }))
    }
}
