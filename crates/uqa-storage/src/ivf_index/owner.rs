//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One owner admits every IVF state transition, including query-triggered training.

use std::sync::Arc;

use uqa_core::{memory::Budgeted, DocId, PostingList};

use super::{IVFData, IVFMetadataSnapshot, IVFMutation, IVFState};
use crate::{
    memory_capture::{
        MemoryCapture, MemoryCaptureCell, MemoryCaptureRegistry, MemoryCaptureSource,
    },
    read_control::StorageReadControl,
    vector_index::{
        image::{VectorImageSnapshot, VectorImageSource},
        query::normalized_query,
        validate_vector_values, IVFIndexParams,
    },
    StorageBackendResult,
};

pub struct IVFIndex {
    pub(super) data: MemoryCaptureCell<IVFData>,
}

impl IVFIndex {
    pub fn new(dimensions: u32) -> Self {
        Self::from_data(IVFData::new(dimensions))
    }

    pub fn with_params(
        dimensions: u32,
        nlist: usize,
        nprobe: usize,
        train_threshold: usize,
    ) -> Self {
        Self::from_data(IVFData::with_params(
            dimensions,
            nlist,
            nprobe,
            train_threshold,
        ))
    }

    pub(super) fn from_data(data: IVFData) -> Self {
        Self {
            data: MemoryCaptureCell::new(data),
        }
    }

    pub(super) fn from_budgeted(data: Budgeted<IVFData>) -> Budgeted<Self> {
        let (data, memory) = data.into_parts();
        Budgeted::new(Self::from_data(data), memory)
    }

    /// Register before publishing this index in the domain. Training, probe changes and ordinary writes then preserve every applicable cut inside this owner.
    pub fn register_memory_capture(
        &mut self,
        registry: &MemoryCaptureRegistry,
    ) -> StorageBackendResult<()> {
        self.data.register(registry)
    }

    /// Retain an export-only image of the selected cut. Querying a stale index must not retrain the archive's saved generation.
    pub fn captured(
        &self,
        capture: &MemoryCapture,
    ) -> StorageBackendResult<Arc<Budgeted<VectorImageSnapshot>>> {
        self.data.captured(capture)
    }

    pub fn state(&self) -> IVFState {
        self.data.read().state
    }
    pub fn nlist(&self) -> usize {
        self.data.read().nlist
    }
    pub fn nprobe(&self) -> usize {
        self.data.read().nprobe
    }
    pub fn set_nprobe(&self, nprobe: usize) {
        self.data.write().set_nprobe(nprobe);
    }
    pub fn metadata_snapshot(&self) -> IVFMetadataSnapshot {
        self.data.read().metadata_snapshot()
    }
    pub fn train(&self) -> StorageBackendResult<()> {
        self.data.write().train()
    }

    pub fn from_persistence(
        dimensions: u32,
        nlist: usize,
        nprobe: usize,
        train_threshold: usize,
        vectors: Vec<(DocId, u32, Vec<f32>)>,
        snapshot: IVFMetadataSnapshot,
    ) -> StorageBackendResult<Self> {
        IVFData::from_persistence(
            dimensions,
            nlist,
            nprobe,
            train_threshold,
            vectors,
            snapshot,
        )
        .map(Self::from_data)
    }

    pub fn restore_controlled(
        dimensions: u32,
        params: IVFIndexParams,
        vectors: Vec<(DocId, u32, Vec<f32>)>,
        snapshot: IVFMetadataSnapshot,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        IVFData::restore_controlled(dimensions, params, vectors, snapshot, control)
            .map(Self::from_budgeted)
    }

    pub fn prepare_metadata(
        &self,
        mutation: IVFMutation<'_>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<IVFMetadataSnapshot>> {
        self.prepare_metadata_changes(std::slice::from_ref(&mutation), control)
    }

    pub(crate) fn prepare_metadata_changes(
        &self,
        mutations: &[IVFMutation<'_>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<IVFMetadataSnapshot>> {
        self.data
            .read_with_control(control)?
            .prepare_metadata_changes(mutations, control)
    }

    pub(crate) fn detached_clone(&self) -> Self {
        Self::from_data(self.data.read().detached_clone())
    }

    pub(super) fn snapshot_controlled(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        self.data
            .read_with_control(control)?
            .snapshot_controlled(control)
            .map(Self::from_budgeted)
    }

    pub(super) fn search_top_k(
        &self,
        query: &[f32],
        k: usize,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<PostingList> {
        let data = match control {
            Some(control) => self.data.read_with_control(control)?,
            None => self.data.read(),
        };
        super::prepare::check(control)?;
        validate_vector_values(data.dimensions, query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        let (normalized, norm) = normalized_query(query, control)?;
        if data.state != IVFState::Stale {
            return data.search_top_k(query, &normalized, norm, k, control);
        }
        drop(data);
        let mut data = match control {
            Some(control) => self.data.write_with_control(control)?,
            None => self.data.write(),
        };
        // Another query may already have published training before this writer was admitted.
        if data.state == IVFState::Stale {
            data.train_for_query(control)?;
        }
        data.search_top_k(query, &normalized, norm, k, control)
    }

    pub(super) fn search_above_threshold(
        &self,
        query: &[f32],
        threshold: f32,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<PostingList> {
        match control {
            Some(control) => self
                .data
                .read_with_control(control)?
                .search_above_threshold(query, threshold, Some(control)),
            None => self
                .data
                .read()
                .search_above_threshold(query, threshold, None),
        }
    }
}

impl VectorImageSource for Budgeted<IVFData> {
    fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        super::image::write(self, output, control)
    }
}

impl MemoryCaptureSource for IVFData {
    type Frozen = VectorImageSnapshot;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        VectorImageSnapshot::new(self.snapshot_controlled(control)?, control)
    }
}
