//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated IVF document changes and provider-owned record codecs.

mod effects;
mod resolve;

use super::VersionResult;
use crate::{
    ivf_index::{IVFMetadataSnapshot, IVFState},
    read_control::StorageReadControl,
    IVFIndexParams,
};
use uqa_core::{memory::BudgetedVec, DocId};

#[derive(Clone, Copy)]
pub struct IVFRecordHeader {
    pub dimensions: u32,
    pub params: IVFIndexParams,
    pub state: IVFState,
    pub trained_size: usize,
    pub deletes_since_train: usize,
    pub vector_count: usize,
    pub revision: u64,
}

#[derive(Clone, Copy)]
pub enum IVFRecordKey {
    Structure,
    Document(DocId),
    Vectors,
    Centroids,
    Assignments,
    Centroid(usize),
    Assignment(DocId, u32),
}

pub enum IVFRecordValue<'a> {
    Header {
        snapshot: &'a IVFMetadataSnapshot,
        revision: u64,
    },
    Centroid(&'a [f32]),
    Assignment(usize),
}

/// Only physical addressing and row codecs. Common storage owns reconstruction, ordered mutation replay, conflicts and publication.
pub trait IVFRecordLayout: Send + Sync {
    /// Return the owning metadata key for a header, centroid or assignment; unrelated records return `None`.
    fn metadata_key(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>>;
    fn key(
        &self,
        metadata: &[u8],
        address: IVFRecordKey,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;
    fn header(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<IVFRecordHeader>;
    /// Decode a canonical vector identity without hydrating its payload.
    fn vector_id(&self, key: &[u8]) -> VersionResult<(DocId, u32)>;
    fn vector(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32, BudgetedVec<f32>)>;
    fn centroid(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(usize, BudgetedVec<f32>)>;
    fn assignment(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<(DocId, u32, usize)>;
    fn encode(
        &self,
        key: &[u8],
        metadata_template: &[u8],
        value: IVFRecordValue<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;
}

pub(super) use effects::{IVFEffects, OwnedIVFMutation};
pub(super) use resolve::resolve;
