//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled generation readers, physical artifact sealing and retained page ownership.

use uqa_core::memory::{BudgetedVec, MemoryError};

use super::format::{DiskANNGeneration, DiskANNManifest};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

mod cache;
mod memory;
mod reader;
mod seal;

pub use memory::{DiskANNMemoryBuilder, DiskANNMemorySource};
pub use reader::{DiskANNPageLease, DiskANNReader};
pub use seal::{DiskANNArtifactSeal, DiskANNArtifactSealer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiskANNRecordKey {
    Manifest,
    Codebook,
    /// Batch beginning at the given dense generation-local node ID.
    Codes(u64),
    /// Batch beginning at the given numeric-side stream position.
    Side(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNReadCapabilities {
    max_batch_pages: usize,
    read_concurrency: usize,
}

impl DiskANNReadCapabilities {
    pub fn new(max_batch_pages: usize, read_concurrency: usize) -> StorageBackendResult<Self> {
        if max_batch_pages == 0 || read_concurrency == 0 || read_concurrency > max_batch_pages {
            return Err(invalid("invalid batch size or read concurrency"));
        }
        Ok(Self {
            max_batch_pages,
            read_concurrency,
        })
    }
    pub fn max_batch_pages(self) -> usize {
        self.max_batch_pages
    }
    pub fn read_concurrency(self) -> usize {
        self.read_concurrency
    }
}

pub type DiskANNRecordVisitor<'a> = dyn FnMut(&[u8]) -> StorageBackendResult<()> + 'a;
pub type DiskANNPageVisitor<'a> = dyn FnMut(u64, &[u8]) -> StorageBackendResult<()> + 'a;

/// One immutable generation and its retention/affinity lease. Visitors are internal copy operations, never application callbacks; provider guards must end before the call returns.
pub trait DiskANNPageSource: Send + Sync {
    fn generation(&self) -> DiskANNGeneration;
    fn capabilities(&self) -> DiskANNReadCapabilities;
    /// Visit exactly one record. Check the byte limit before materializing provider-owned bytes; absence is an error.
    fn read_record(
        &self,
        key: DiskANNRecordKey,
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()>;
    /// Visit every requested unique page once, in any completion order. Missing, extra and repeated completions are errors.
    fn read_graph_pages(
        &self,
        pages: &[u64],
        control: &StorageReadControl,
        visit: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNReadLimits {
    /// Retained reader metadata, decoded codebook and resident code bytes.
    pub resident_bytes: usize,
    /// Cache payloads and bookkeeping, including externally pinned evicted pages.
    pub cache_bytes: usize,
    /// Caller-owned page buffers in one source batch. Provider workspace also charges the invoking allowance.
    pub max_in_flight_page_bytes: usize,
    /// Maximum encoded size of one metadata record, checked before provider materialization and reader copying.
    pub max_record_bytes: usize,
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid DiskANN page source: {message}"))
}

fn copy(
    bytes: &[u8],
    limit: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    control.check()?;
    if bytes.len() > limit {
        return Err(MemoryError::Limit {
            required: bytes.len(),
            limit,
        }
        .into());
    }
    let mut result = BudgetedVec::new(control.memory());
    result.reserve(bytes.len())?;
    for part in bytes.chunks(4096) {
        control.check()?;
        result.extend_from_slice(part)?;
    }
    control.check()?;
    Ok(result)
}

fn read_record(
    source: &dyn DiskANNPageSource,
    key: DiskANNRecordKey,
    limit: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    control.check()?;
    let budget = control.memory().child(limit);
    let read_control = StorageReadControl::new(&budget, control.cancellation());
    let mut result = None;
    let mut failure = None;
    let source_result = source.read_record(key, limit, control, &mut |bytes| {
        if failure.is_some() {
            return Err(invalid("record visitor already failed"));
        }
        let outcome = if result.is_some() {
            Err(invalid("record was returned more than once"))
        } else {
            copy(bytes, limit, &read_control).map(|bytes| result = Some(bytes))
        };
        if let Err(error) = outcome {
            failure = Some(error);
            return Err(invalid("record visitor rejected data"));
        }
        Ok(())
    });
    if let Some(error) = failure {
        return Err(error);
    }
    source_result?;
    control.check()?;
    result.ok_or_else(|| invalid("record was not returned"))
}

fn check_catalog(
    manifest: &DiskANNManifest,
    dimensions: u32,
    parameters: crate::vector_index::DiskANNIndexParams,
) -> StorageBackendResult<()> {
    parameters.validate(dimensions)?;
    if manifest.input().dimensions != dimensions || manifest.input().parameters != parameters {
        return Err(invalid(
            "manifest differs from the selected catalog definition",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
