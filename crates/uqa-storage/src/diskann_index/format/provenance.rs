//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::{field, invalid, record, DiskANNBuildCoverage};
use crate::diskann_index::build::{
    DiskANNMergeOptions, DiskANNMergeSummary, DiskANNPartitionOptions,
};
use crate::diskann_index::PQTrainingOptions;
use crate::{vector_index::DiskANNIndexParams, StorageBackendResult};

/// Compact, versioned construction settings and fingerprints. The enclosing manifest supplies generation, canonical coverage and index parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNBuildProvenance {
    work_revision: u32,
    partition_revision: u32,
    partition_options: DiskANNPartitionOptions,
    partitions: u64,
    memberships: u64,
    partition_edges: u64,
    maximum_depth: u8,
    maximum_partition_points: usize,
    merge_revision: u32,
    merge_options: DiskANNMergeOptions,
    merge_passes: u8,
    edges: u64,
    training: PQTrainingOptions,
    code_batch_nodes: usize,
    side_batch_entries: usize,
    entry_sample_points: u64,
    assignment_digest: [u8; 32],
    adjacency_digest: [u8; 32],
}

impl DiskANNBuildProvenance {
    pub const ENCODED_BYTES: usize = 256;

    pub fn from_merge(
        summary: &DiskANNMergeSummary,
        nodes: u64,
        training: PQTrainingOptions,
        code_batch_nodes: usize,
        side_batch_entries: usize,
    ) -> StorageBackendResult<Self> {
        let partition = &summary.partitions;
        let value = Self {
            work_revision: 1,
            partition_revision: partition.work_order_revision,
            partition_options: partition.options,
            partitions: partition.partitions,
            memberships: partition.memberships,
            partition_edges: partition.edges,
            maximum_depth: partition.maximum_depth,
            maximum_partition_points: partition.maximum_partition_points,
            merge_revision: summary.work_order_revision,
            merge_options: summary.options,
            merge_passes: summary.merge_passes,
            edges: summary.edges,
            training,
            code_batch_nodes,
            side_batch_entries,
            entry_sample_points: nodes.min(256),
            assignment_digest: partition.assignment_digest,
            adjacency_digest: summary.adjacency_digest,
        };
        value.validate(nodes, partition.parameters)?;
        Ok(value)
    }

    pub fn training(&self) -> PQTrainingOptions {
        self.training
    }
    pub fn code_batch_nodes(&self) -> usize {
        self.code_batch_nodes
    }
    pub fn side_batch_entries(&self) -> usize {
        self.side_batch_entries
    }
    pub fn entry_sample_points(&self) -> u64 {
        self.entry_sample_points
    }
    pub fn edges(&self) -> u64 {
        self.edges
    }
    pub fn adjacency_digest(&self) -> [u8; 32] {
        self.adjacency_digest
    }

    pub(super) fn validate(
        &self,
        nodes: u64,
        parameters: DiskANNIndexParams,
    ) -> StorageBackendResult<()> {
        self.partition_options.validate()?;
        self.training.validate()?;
        let empty = nodes == 0;
        let maximum_edges = u128::from(nodes)
            * (parameters.max_degree as u128).min(u128::from(nodes.saturating_sub(1)));
        let invalid_empty = empty
            && (self.partitions != 0
                || self.memberships != 0
                || self.partition_edges != 0
                || self.maximum_depth != 0
                || self.maximum_partition_points != 0);
        let invalid_populated = !empty
            && (self.partitions == 0
                || self.memberships < nodes
                || self.partitions > self.memberships
                || self.maximum_partition_points == 0);
        let mut width = self.merge_options.sort_buffer_records as u64;
        let mut passes = 0;
        if width == 0 {
            return Err(invalid("zero build sort capacity"));
        }
        while width < self.partition_edges {
            width = width.saturating_mul(2).min(self.partition_edges);
            passes += 1;
        }
        if self.work_revision != 1
            || self.partition_revision != DiskANNPartitionOptions::WORK_ORDER_REVISION
            || self.merge_revision != 1
            || self.merge_passes != passes
            || invalid_empty
            || invalid_populated
            || self.maximum_depth > self.partition_options.max_depth
            || self.maximum_partition_points > self.partition_options.max_partition_points
            || self.maximum_partition_points as u64 > nodes
            || (nodes == 1
                && (self.partitions != 1
                    || self.memberships != 1
                    || self.partition_edges != 0
                    || self.maximum_depth != 0))
            || u128::from(self.memberships)
                > u128::from(self.partitions) * self.maximum_partition_points as u128
            || u128::from(self.partition_edges)
                > u128::from(self.memberships) * parameters.max_degree as u128
            || u128::from(self.edges) > maximum_edges
            || (nodes > 1 && self.edges < nodes)
            || self.training.seed != parameters.seed
            || self.code_batch_nodes == 0
            || self.side_batch_entries == 0
            || self.entry_sample_points != nodes.min(256)
        {
            return Err(invalid("inconsistent or unsupported build provenance"));
        }
        Ok(())
    }

    pub(super) fn encode(&self, bytes: &mut BudgetedVec<u8>) -> StorageBackendResult<()> {
        let options = self.partition_options;
        let coarse = options.coarse_training;
        for value in [
            u64::from(self.work_revision),
            u64::from(self.partition_revision),
            options.max_partition_points as u64,
            u64::from(options.max_depth),
            u64::from(coarse.max_samples),
            u64::from(coarse.max_iterations),
            u64::from(coarse.max_centroids),
            coarse.seed,
            self.partitions,
            self.memberships,
            self.partition_edges,
            u64::from(self.maximum_depth),
            self.maximum_partition_points as u64,
            u64::from(self.merge_revision),
            self.merge_options.sort_buffer_records as u64,
            u64::from(self.merge_passes),
            self.edges,
            u64::from(self.training.max_samples),
            u64::from(self.training.max_iterations),
            u64::from(self.training.max_centroids),
            self.training.seed,
            self.code_batch_nodes as u64,
            self.side_batch_entries as u64,
            self.entry_sample_points,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes())?;
        }
        bytes.extend_from_slice(&self.assignment_digest)?;
        bytes.extend_from_slice(&self.adjacency_digest)?;
        Ok(())
    }

    pub(super) fn decode(
        bytes: &[u8],
        nodes: u64,
        parameters: DiskANNIndexParams,
    ) -> StorageBackendResult<Self> {
        if bytes.len() != Self::ENCODED_BYTES {
            return Err(invalid("build provenance size differs"));
        }
        let value = Self {
            work_revision: number(bytes, 0)?,
            partition_revision: number(bytes, 1)?,
            partition_options: DiskANNPartitionOptions {
                max_partition_points: number(bytes, 2)?,
                max_depth: number(bytes, 3)?,
                coarse_training: PQTrainingOptions {
                    max_samples: number(bytes, 4)?,
                    max_iterations: number(bytes, 5)?,
                    max_centroids: number(bytes, 6)?,
                    seed: number(bytes, 7)?,
                },
            },
            partitions: number(bytes, 8)?,
            memberships: number(bytes, 9)?,
            partition_edges: number(bytes, 10)?,
            maximum_depth: number(bytes, 11)?,
            maximum_partition_points: number(bytes, 12)?,
            merge_revision: number(bytes, 13)?,
            merge_options: DiskANNMergeOptions {
                sort_buffer_records: number(bytes, 14)?,
            },
            merge_passes: number(bytes, 15)?,
            edges: number(bytes, 16)?,
            training: PQTrainingOptions {
                max_samples: number(bytes, 17)?,
                max_iterations: number(bytes, 18)?,
                max_centroids: number(bytes, 19)?,
                seed: number(bytes, 20)?,
            },
            code_batch_nodes: number(bytes, 21)?,
            side_batch_entries: number(bytes, 22)?,
            entry_sample_points: number(bytes, 23)?,
            assignment_digest: field(bytes, 192)?,
            adjacency_digest: field(bytes, 224)?,
        };
        value.validate(nodes, parameters)?;
        Ok(value)
    }
}

pub(in crate::diskann_index) fn adjacency_hash(
    coverage: DiskANNBuildCoverage,
    nodes: u64,
    degree: usize,
) -> Sha256 {
    let mut hash = Sha256::new();
    hash.update(b"UQA DiskANN adjacency\0\x01");
    hash.update(coverage.digest());
    hash.update(nodes.to_le_bytes());
    hash.update((degree as u64).to_le_bytes());
    hash
}

fn number<T: TryFrom<u64>>(bytes: &[u8], index: usize) -> StorageBackendResult<T> {
    T::try_from(record::u64_at(bytes, index * 8)?)
        .map_err(|_| invalid("build provenance integer range"))
}
