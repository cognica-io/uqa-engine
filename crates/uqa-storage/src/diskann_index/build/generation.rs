//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::{invalid, DiskANNBuildInput, DiskANNMergedGraph};
use crate::diskann_index::format::{
    encode_page, DiskANNArtifactDigests, DiskANNBuildProvenance, DiskANNGeneration,
    DiskANNManifest, DiskANNManifestInput, DiskANNNodeInput, DiskANNNodeLayout, DiskANNSideEntry,
    DiskANNSideLayout, PAGE_HEADER_BYTES, PAGE_PAYLOAD_BYTES,
};
use crate::diskann_index::pages::{DiskANNMemoryBuilder, DiskANNRecordKey};
use crate::diskann_index::PQTrainingOptions;
use crate::{
    key_value::KeyValueDiskANNStage, read_control::StorageReadControl, StorageBackendResult,
};

#[cfg(test)]
mod tests;
mod vectors;

/// Effective bounded metadata batches. Graph pages keep their codec-defined size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNGenerationOptions {
    pub training: PQTrainingOptions,
    pub code_batch_nodes: usize,
    pub side_batch_entries: usize,
    pub max_record_bytes: usize,
}

/// A caller-retained unpublished artifact owner. A failed build leaves physical recovery and pending transaction outcomes with this owner; the algorithm never publishes or discards it implicitly.
pub trait DiskANNBuildSink {
    fn generation(&self) -> DiskANNGeneration;
    fn write_record(
        &mut self,
        key: DiskANNRecordKey,
        bytes: &[u8],
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
    fn write_graph_page(
        &mut self,
        id: u64,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()>;
}

impl DiskANNBuildSink for DiskANNMemoryBuilder {
    fn generation(&self) -> DiskANNGeneration {
        self.generation()
    }
    fn write_record(
        &mut self,
        key: DiskANNRecordKey,
        bytes: &[u8],
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check_value_size(bytes.len(), maximum)?;
        DiskANNMemoryBuilder::write_record(self, key, bytes, control)
    }
    fn write_graph_page(
        &mut self,
        id: u64,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        DiskANNMemoryBuilder::write_graph_page(self, id, bytes, control)
    }
}

impl DiskANNBuildSink for KeyValueDiskANNStage {
    fn generation(&self) -> DiskANNGeneration {
        self.generation()
    }
    fn write_record(
        &mut self,
        key: DiskANNRecordKey,
        bytes: &[u8],
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        KeyValueDiskANNStage::write_record(self, key, bytes, maximum, control)
    }
    fn write_graph_page(
        &mut self,
        id: u64,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        KeyValueDiskANNStage::write_graph_page(self, id, bytes, control)
    }
}

impl DiskANNBuildInput {
    /// Stream immutable records to the selected private generation. The returned manifest must pass the sink owner's physical seal before opening; it grants no SQL/MVCC publication authority.
    pub fn write_generation(
        &self,
        graph: &DiskANNMergedGraph,
        options: DiskANNGenerationOptions,
        sink: &mut dyn DiskANNBuildSink,
    ) -> StorageBackendResult<DiskANNManifest> {
        graph.check_input(self)?;
        let generation = self.coverage.generation();
        let parameters = graph.summary().partitions.parameters;
        if sink.generation() != generation {
            return Err(invalid("artifact sink generation differs"));
        }
        self.control
            .check_value_size(DiskANNManifest::MAX_ENCODED_BYTES, options.max_record_bytes)?;
        let provenance = DiskANNBuildProvenance::from_merge(
            graph.summary(),
            self.node_count(),
            options.training,
            options.code_batch_nodes,
            options.side_batch_entries,
        )?;
        let mut artifacts = DiskANNArtifactDigests::empty();
        vectors::quantize(self, parameters.pq_bytes, options, sink, &mut artifacts)?;
        let entry_node = vectors::entry(self, parameters.seed)?;
        artifacts.side = self.write_sides(options, sink)?;
        artifacts.graph = self.write_graph(graph, sink)?;
        self.control.check()?;
        DiskANNManifest::new(DiskANNManifestInput {
            generation,
            dimensions: self.dimensions,
            parameters,
            nodes: self.node_count(),
            side_vectors: self.side_count(),
            entry_node,
            coverage: self.coverage,
            artifacts,
        })?
        .with_build_provenance(provenance)
    }

    fn write_sides(
        &self,
        options: DiskANNGenerationOptions,
        sink: &mut dyn DiskANNBuildSink,
    ) -> StorageBackendResult<[u8; 32]> {
        let layout = DiskANNSideLayout::new(
            self.coverage.generation(),
            self.dimensions,
            self.side_count(),
        )?;
        let capacity = self.side_count().min(options.side_batch_entries as u64) as usize;
        let mut entries = BudgetedVec::new(self.control.memory());
        entries.reserve(capacity)?;
        let mut first = 0;
        let mut hash = Sha256::new();
        for id in 0..self.side_count() {
            let record = self.read_side(id)?;
            entries.push(DiskANNSideEntry::from_raw(
                self.dimensions,
                record.doc_id(),
                record.ordinal(),
                record.version(),
                record.raw(),
                &self.control,
            )?)?;
            if entries.len() == capacity || id + 1 == self.side_count() {
                let bytes = layout.encode(first, &entries, &self.control)?;
                self.control
                    .check_value_size(bytes.len(), options.max_record_bytes)?;
                let batch = layout.decode(first, &bytes, &self.control)?;
                for chunk in batch.bytes().chunks(4096) {
                    self.control.check()?;
                    hash.update(chunk);
                }
                sink.write_record(
                    DiskANNRecordKey::Side(first),
                    &bytes,
                    options.max_record_bytes,
                    &self.control,
                )?;
                first = id + 1;
                entries.clear();
            }
        }
        Ok(hash.finalize().into())
    }

    fn write_graph(
        &self,
        graph: &DiskANNMergedGraph,
        sink: &mut dyn DiskANNBuildSink,
    ) -> StorageBackendResult<[u8; 32]> {
        let layout = DiskANNNodeLayout::new(
            self.dimensions,
            graph.summary().partitions.parameters.max_degree,
            self.node_count(),
        )?;
        let generation = self.coverage.generation();
        let mut payload = BudgetedVec::new(self.control.memory());
        let mut page = 0;
        let mut hash = Sha256::new();
        graph.visit_neighbors(&mut |node_id, neighbors| {
            let record = self.read_node(node_id)?;
            let encoded = layout.encode_node(
                &DiskANNNodeInput {
                    node_id,
                    doc_id: record.doc_id(),
                    ordinal: record.ordinal(),
                    version: record.version(),
                    vector: record.raw(),
                    neighbors,
                },
                &self.control,
            )?;
            let expected = layout.page_shape(page)?.payload_bytes as usize;
            let mut emit = |payload: &[u8]| -> StorageBackendResult<()> {
                let bytes = encode_page(generation, layout, page, payload, &self.control)?;
                hash.update(&bytes[PAGE_HEADER_BYTES - 32..PAGE_HEADER_BYTES]);
                sink.write_graph_page(page, &bytes, &self.control)?;
                page += 1;
                Ok(())
            };
            if layout.node_address(node_id)?.fragments > 1 {
                for fragment in encoded.chunks(PAGE_PAYLOAD_BYTES) {
                    self.control.check()?;
                    emit(fragment)?;
                }
            } else {
                if payload.is_empty() {
                    payload.reserve(expected)?;
                }
                payload.extend_from_slice(&encoded)?;
                if payload.len() == expected {
                    emit(&payload)?;
                    payload.clear();
                }
            }
            Ok(())
        })?;
        if page != layout.page_count() || !payload.is_empty() {
            return Err(invalid("incomplete graph page output"));
        }
        Ok(hash.finalize().into())
    }
}
