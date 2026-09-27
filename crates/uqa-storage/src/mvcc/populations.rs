//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical population reconciliation belongs to common MVCC; providers supply addressing, codecs and fixed-view canonical adapters.

mod effects;
mod reconcile;
mod resolve;

pub(super) use effects::{OwnedPopulationMutation, PopulationEffects};
pub(super) use resolve::{resolve, stage};

use super::{DatabaseId, MergedRecordSnapshot, VersionResult};
use crate::diskann_index::{
    format::DiskANNCanonicalOrigin, DiskANNPopulationState, DiskANNPopulationWitness,
    DiskANNQueryRead,
};
use crate::read_control::StorageReadControl;
use uqa_core::{memory::BudgetedVec, DocId};

/// An evaluated origin and its provider-owned canonical field address.
pub struct DiskANNPopulationOrigin {
    pub field: BudgetedVec<u8>,
    pub document: DocId,
    pub origin: DiskANNCanonicalOrigin,
}

/// A validated population header with its provider-owned canonical field address.
pub struct DiskANNPopulationHeader {
    pub field: BudgetedVec<u8>,
    pub state: DiskANNPopulationState,
}

/// Only physical mapping and retained canonical adapters. Common storage owns census, replacement arithmetic, concurrency, private previews and lifecycle resolution.
pub trait DiskANNPopulationRecordLayout: Send + Sync {
    fn origin(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<DiskANNPopulationOrigin>;

    /// Resolve a raw invalidation's owning field without requiring an existing origin value.
    fn origin_header_prefix(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    /// Select headers only, for every active generation associated with this exact field.
    fn header_prefix(
        &self,
        field: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    fn header(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<DiskANNPopulationHeader>;

    fn witness_prefix(
        &self,
        header: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    /// Map a header or witness key back to its owning generation's header; reject unrelated records.
    fn preview_header(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    fn witness_key(
        &self,
        header: &[u8],
        document: DocId,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    fn witness(
        &self,
        key: &[u8],
        value: &[u8],
        state: DiskANNPopulationState,
        document: DocId,
        origin: DiskANNCanonicalOrigin,
        control: &StorageReadControl,
    ) -> VersionResult<DiskANNPopulationWitness>;

    fn encode_header(
        &self,
        key: &[u8],
        template: &[u8],
        state: DiskANNPopulationState,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    fn encode_witness(
        &self,
        key: &[u8],
        header_template: &[u8],
        witness: DiskANNPopulationWitness,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    /// Invoke exactly once, outside provider locks, with the existing complete-origin/ordinal validator on this fixed view. Do not reconstruct coordinates or advance a session.
    fn with_canonical(
        &self,
        field: &[u8],
        dimensions: u32,
        view: &MergedRecordSnapshot,
        history: DatabaseId,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(&dyn DiskANNQueryRead) -> VersionResult<()>,
    ) -> VersionResult<()>;
}
