//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::DiskANNQueryMetadata;
use crate::diskann_index::{
    format::PAGE_BYTES,
    metric::{exact_reason, norms},
    ExactVectorReason,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::{
    DiskANNQueryStats, VectorGeneration, VectorPopulationStats, VectorQueryRoute, VectorReadStats,
};

impl DiskANNQueryMetadata {
    /// Project stored physical facts and classify the query with the same canonical norm calculation as execution, without normalizing coordinates or reading artifact bodies.
    pub fn query_statistics(
        &self,
        query: &[f32],
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNQueryStats> {
        let input = self.manifest.input();
        let (norm, _) = norms(input.dimensions, query, control)?;
        let query_route = match exact_reason(norm) {
            None => VectorQueryRoute::Approximate,
            Some(ExactVectorReason::ZeroNorm) => VectorQueryRoute::ExactZeroNorm,
            Some(ExactVectorReason::NonFiniteNorm) => VectorQueryRoute::ExactNonFiniteNorm,
        };
        let generation = input.generation;
        let layout = self.manifest.layout();
        let pq_centroids = if input.nodes == 0 {
            Some(0)
        } else {
            self.manifest.build_provenance().map(|provenance| {
                let options = provenance.training();
                input
                    .nodes
                    .min(u64::from(options.max_samples))
                    .min(u64::from(options.max_centroids)) as u16
            })
        };
        control.check()?;
        Ok(DiskANNQueryStats {
            generation: VectorGeneration {
                database: generation.database(),
                table: generation.table(),
                index: generation.index(),
                generation: generation.generation(),
            },
            dimensions: input.dimensions,
            max_degree: input.parameters.max_degree,
            search_list_size: input.parameters.search_list_size,
            beam_width: input.parameters.beam_width,
            pq_bytes: input.parameters.pq_bytes,
            pq_centroids,
            node_slot_bytes: layout.slot_bytes(),
            node_fragments: if input.nodes == 0 {
                0
            } else {
                layout.node_address(0)?.fragments
            },
            graph_pages: layout.page_count(),
            graph_edges: self
                .manifest
                .build_provenance()
                .map(crate::diskann_index::format::DiskANNBuildProvenance::edges),
            page_bytes: PAGE_BYTES,
            populations: VectorPopulationStats {
                base_documents: self
                    .manifest
                    .origins()
                    .map(crate::diskann_index::format::DiskANNOriginSummary::documents),
                base_vectors: input.coverage.vector_count(),
                graph_nodes: input.nodes,
                side_vectors: input.side_vectors,
                current_vectors: None,
                changed_vectors: None,
            },
            reads: VectorReadStats {
                resident_bytes: self.read_limits.resident_bytes,
                cache_bytes: self.read_limits.cache_bytes,
                max_record_bytes: self.read_limits.max_record_bytes,
                max_in_flight_page_bytes: self.read_limits.max_in_flight_page_bytes,
                max_batch_pages: self.read_capabilities.max_batch_pages(),
                read_concurrency: self.read_capabilities.read_concurrency(),
            },
            query_route,
        })
    }
}
