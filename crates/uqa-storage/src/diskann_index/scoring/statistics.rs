//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete tensor scoring work is returned by the invocation that performed it.

use crate::StorageBackendResult;
use uqa_core::memory::MemoryError;

pub use uqa_core::vector_execution::DiskANNScoringStats;

pub(crate) trait ScoringStatsExt {
    fn record(&mut self, vectors: u64) -> StorageBackendResult<()>;
}

impl ScoringStatsExt for DiskANNScoringStats {
    fn record(&mut self, vectors: u64) -> StorageBackendResult<()> {
        *self = Self {
            documents: self
                .documents
                .checked_add(u64::from(vectors != 0))
                .ok_or(MemoryError::SizeOverflow)?,
            vectors: self
                .vectors
                .checked_add(vectors)
                .ok_or(MemoryError::SizeOverflow)?,
        };
        Ok(())
    }
}
