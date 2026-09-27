//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete tensor scoring work is returned by the invocation that performed it.

use crate::StorageBackendResult;
use uqa_core::memory::MemoryError;

/// Successful vector-bearing tensor scores and their actual cosine evaluations. Repeated scoring is repeated work, not a distinct-document census.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskANNScoringStats {
    pub documents: u64,
    pub vectors: u64,
}

impl DiskANNScoringStats {
    pub(crate) fn record(&mut self, vectors: u64) -> StorageBackendResult<()> {
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
