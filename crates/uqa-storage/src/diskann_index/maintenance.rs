//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact outstanding-change accounting and construction share one retained canonical snapshot.

use super::{
    build::DiskANNTemporaryBudget,
    changes::{classify, Change, DiskANNChangeRead, DiskANNJournalPruner},
    format::{DiskANNChangeIdentity, DiskANNGeneration},
    pages::DiskANNOriginReader,
    DiskANNIndexOptions,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};

/// Logical work in one captured view. Empty replacements count as documents; vector bytes count raw f32 coordinates, not physical journal, compressed-file or MVCC-history bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskANNChangeStatistics {
    pub documents: u64,
    pub vectors: u64,
    pub vector_bytes: u64,
}

impl DiskANNChangeStatistics {
    /// Combine disjoint pages from the same captured source exactly once; an overflow invalidates the entire census.
    pub fn checked_add(self, other: Self) -> StorageBackendResult<Self> {
        let add = |left: u64, right: u64| {
            left.checked_add(right)
                .ok_or_else(|| invalid("outstanding change statistics overflow"))
        };
        Ok(Self {
            documents: add(self.documents, other.documents)?,
            vectors: add(self.vectors, other.vectors)?,
            vector_bytes: add(self.vector_bytes, other.vector_bytes)?,
        })
    }
}

/// Continue on the same maintenance source. A fresh source always starts without a cursor, even when it selects the same generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNStatisticsCursor {
    generation: DiskANNGeneration,
    after: DiskANNChangeIdentity,
}

#[derive(Debug, Clone, Copy)]
pub struct DiskANNStatisticsRequest {
    pub after: Option<DiskANNStatisticsCursor>,
    pub max_records: usize,
}

/// One bounded, read-only census page. Totals describe the captured view, not later committed writes or physical retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNStatisticsPage {
    pub generation: DiskANNGeneration,
    pub examined: usize,
    pub outstanding: DiskANNChangeStatistics,
    pub next: Option<DiskANNStatisticsCursor>,
}

/// An actual provider-owned catalog/canonical capture. Statistics and construction use this same committed view, allowance and cancellation signal. Rebuilding consumes the source once and stages publication in its original session's active transaction; the caller must resolve that transaction without replaying construction.
pub trait DiskANNMaintenanceSource: DiskANNJournalPruner {
    fn statistics(
        &self,
        request: DiskANNStatisticsRequest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNStatisticsPage>;

    fn rebuild(
        self: Box<Self>,
        options: DiskANNIndexOptions,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
}

pub(crate) fn measure(
    origins: &DiskANNOriginReader,
    journal: &dyn DiskANNChangeRead,
    request: DiskANNStatisticsRequest,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNStatisticsPage> {
    control.check()?;
    let generation = origins.manifest().input().generation;
    if request.max_records == 0
        || request
            .after
            .is_some_and(|cursor| cursor.generation != generation)
    {
        return Err(invalid("invalid statistics limit or generation cursor"));
    }
    let mut after = request.after.map(|cursor| cursor.after);
    let mut page = DiskANNStatisticsPage {
        generation,
        examined: 0,
        outstanding: DiskANNChangeStatistics::default(),
        next: None,
    };
    for _ in 0..request.max_records.min(64) {
        control.check()?;
        let Some(identity) = journal.next_after(after, control)? else {
            page.next = None;
            control.check()?;
            return Ok(page);
        };
        if after.is_some_and(|last| last.encode() >= identity.encode()) {
            return Err(invalid("journal statistics cursor did not advance"));
        }
        match classify(origins, journal, identity, control)? {
            Change::Missing => return Err(invalid("captured journal key has no captured value")),
            Change::Reclaimable => {}
            Change::Outstanding(origin) => {
                let vector_bytes = origin
                    .count()
                    .checked_mul(u64::from(origin.dimensions()))
                    .and_then(|bytes| bytes.checked_mul(4))
                    .ok_or_else(|| invalid("outstanding vector byte count overflow"))?;
                page.outstanding = page.outstanding.checked_add(DiskANNChangeStatistics {
                    documents: 1,
                    vectors: origin.count(),
                    vector_bytes,
                })?;
            }
        }
        page.examined += 1;
        after = Some(identity);
        page.next = Some(DiskANNStatisticsCursor {
            generation,
            after: identity,
        });
    }
    control.check()?;
    Ok(page)
}

fn invalid(message: &'static str) -> crate::StorageBackendError {
    crate::mvcc::VersionError::InvalidEncoding(message).into_storage_error()
}
