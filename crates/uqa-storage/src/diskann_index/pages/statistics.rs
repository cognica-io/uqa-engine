//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Successful page work belongs to one read invocation, never a shared-cache counter delta.

use crate::StorageBackendResult;
use uqa_core::memory::MemoryError;

pub use uqa_core::vector_execution::DiskANNPageReadStats;

pub(crate) trait PageReadStatsExt {
    fn merge(&mut self, other: Self) -> StorageBackendResult<()>;
}

impl PageReadStatsExt for DiskANNPageReadStats {
    fn merge(&mut self, other: Self) -> StorageBackendResult<()> {
        let add = |a: u64, b: u64| a.checked_add(b).ok_or(MemoryError::SizeOverflow);
        *self = Self {
            page_requests: add(self.page_requests, other.page_requests)?,
            cache_hits: add(self.cache_hits, other.cache_hits)?,
            provider_pages: add(self.provider_pages, other.provider_pages)?,
            provider_batches: add(self.provider_batches, other.provider_batches)?,
        };
        Ok(())
    }
}
