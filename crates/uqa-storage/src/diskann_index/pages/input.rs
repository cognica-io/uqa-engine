//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation owns the same immutable source that the completed reader retains.

use std::sync::Arc;

use super::{copy_from, read_record, DiskANNMemorySource, DiskANNPageSource, DiskANNRecordKey};
use crate::{image_io::work::Work, read_control::StorageReadControl, StorageBackendResult};
use uqa_core::memory::BudgetedVec;

#[derive(Clone)]
pub(in crate::diskann_index) enum RecordInput {
    Provider(Arc<dyn DiskANNPageSource>),
    Memory(Arc<DiskANNMemorySource>),
}

impl RecordInput {
    pub(in crate::diskann_index) fn source(&self) -> &dyn DiskANNPageSource {
        match self {
            Self::Provider(source) => &**source,
            Self::Memory(source) => &**source,
        }
    }

    pub(in crate::diskann_index) fn into_source(self) -> Arc<dyn DiskANNPageSource> {
        match self {
            Self::Provider(source) => source,
            Self::Memory(source) => source,
        }
    }

    pub(in crate::diskann_index) async fn read_record(
        &self,
        key: DiskANNRecordKey,
        limit: usize,
        control: &StorageReadControl,
        work: &mut (impl Work + Send),
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        match self {
            Self::Provider(source) => read_record(&**source, key, limit, control),
            Self::Memory(source) => {
                let bytes = source.record(key, limit, control)?;
                let budget = control.memory().child(limit);
                let read_control = StorageReadControl::new(&budget, control.cancellation());
                let bytes = copy_from(bytes, limit, &read_control, work).await?;
                control.check()?;
                Ok(bytes)
            }
        }
    }
}
