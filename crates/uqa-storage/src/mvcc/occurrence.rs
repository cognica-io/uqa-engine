//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider record codecs for the shared occurrence commit resolver.

mod clusters;
mod resolve;
mod statistics;
#[cfg(test)]
mod tests;

use crate::{inverted_index::IndexedFieldRevision, read_control::StorageReadControl};
use uqa_core::memory::BudgetedVec;

use super::VersionResult;

/// Complete record identities eligible for an evaluated occurrence replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OccurrenceRecordKind {
    Score(u64),
    Positions,
    Cluster(u64),
    Statistics,
    Format,
    Cache,
}

/// Related records in the same stable index scope. Cache addresses are scan prefixes; all other addresses are complete keys.
#[derive(Clone, Copy)]
pub enum OccurrenceRelatedKey {
    Structure,
    Format,
    Score,
    Positions,
    Skips,
    BlockMax,
}

/// Borrowed column projections preserve provider payload ownership during decoding. Common storage owns cluster merging and checked field-counter arithmetic.
pub enum OccurrenceRecordValue<'a> {
    Score(&'a [u8]),
    Positions(&'a [u8]),
    Cluster {
        score: &'a [u8],
        positions: &'a [u8],
    },
    Statistics {
        revision: IndexedFieldRevision,
        doc_count: u64,
        total_length: u64,
    },
    Format(&'a [u8]),
}

/// Physical addressing and row codecs only. Methods must validate complete keys, keep related addresses within the original object generation and charge temporary allocations to `control`. They perform no I/O, index algorithms or application callbacks.
pub trait OccurrenceRecordLayout: Send + Sync {
    fn kind(&self, key: &[u8], control: &StorageReadControl)
        -> VersionResult<OccurrenceRecordKind>;

    fn related_key(
        &self,
        key: &[u8],
        related: OccurrenceRelatedKey,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    fn decode<'a>(
        &self,
        key: &[u8],
        value: &'a [u8],
        control: &StorageReadControl,
    ) -> VersionResult<OccurrenceRecordValue<'a>>;

    /// Encode the merged value with the key and immutable row fields from an original, evaluated or current record. This may enforce narrower native scalar bounds before any publication occurs.
    fn encode(
        &self,
        key: &[u8],
        template: &[u8],
        value: OccurrenceRecordValue<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;
}

pub(super) use resolve::resolve;
