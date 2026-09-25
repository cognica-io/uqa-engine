//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use std::path::Path;

use super::runs::{RunReader, RunWriter};
use super::temporary::TemporaryRun;
use super::{invalid, DiskANNBuildCoverage, DiskANNBuildInput};
use crate::diskann_index::PQTrainingOptions;
use crate::{
    read_control::StorageReadControl, vector_index::DiskANNIndexParams, StorageBackendResult,
};

mod leaf;
mod source;
#[cfg(test)]
mod tests;

use source::Ids;

/// Effective construction settings, retained with the resulting provenance. Limits do not silently change sample sizes or partition capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNPartitionOptions {
    pub max_partition_points: usize,
    pub coarse_training: PQTrainingOptions,
    pub max_depth: u8,
}

impl DiskANNPartitionOptions {
    pub const WORK_ORDER_REVISION: u32 = 1;

    fn validate(self) -> StorageBackendResult<Self> {
        self.coarse_training.validate()?;
        if self.max_partition_points < 2
            || self.max_depth > 32
            || self.coarse_training.max_centroids < 3
            || self.coarse_training.max_samples < u32::from(self.coarse_training.max_centroids)
        {
            return Err(invalid("partition capacity must be at least two, depth at most 32 and coarse samples must cover at least three centroids"));
        }
        Ok(self)
    }
}

/// Compact construction provenance, not a physical seal or publication permission. The final generation builder must encode it with its manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNPartitionSummary {
    pub work_order_revision: u32,
    pub options: DiskANNPartitionOptions,
    pub parameters: DiskANNIndexParams,
    pub coverage: DiskANNBuildCoverage,
    pub partitions: u64,
    pub memberships: u64,
    pub edges: u64,
    pub maximum_depth: u8,
    pub maximum_partition_points: usize,
    pub assignment_digest: [u8; 32],
}

/// Encrypted, deterministic global edge candidates from overlapping local graphs. Their union still requires external ordering, final pruning and global connectivity.
pub struct DiskANNPartitionRuns {
    edges: TemporaryRun,
    summary: DiskANNPartitionSummary,
    nodes: u64,
    control: StorageReadControl,
}

impl DiskANNPartitionRuns {
    pub fn summary(&self) -> &DiskANNPartitionSummary {
        &self.summary
    }

    pub(super) fn into_source(
        self,
        input: &DiskANNBuildInput,
    ) -> StorageBackendResult<(TemporaryRun, DiskANNPartitionSummary)> {
        input.control.check()?;
        if self.nodes != input.node_count()
            || self.summary.coverage != input.coverage
            || !self.control.shares_context(&input.control)
            || !self.edges.uses_allowance(&input.temporary)
        {
            return Err(invalid(
                "partition source, coverage or build allowance differs",
            ));
        }
        Ok((self.edges, self.summary))
    }

    pub fn visit_edges(
        &self,
        visitor: &mut dyn FnMut(u64, u64) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.edges.read(&self.control, |file| {
            if self.summary.edges == 0 {
                return Ok(());
            }
            let mut reader = RunReader::new(file, &self.control)?;
            for _ in 0..self.summary.edges {
                let bytes = reader.record::<16>()?;
                let source = u64::from_le_bytes(bytes[..8].try_into().expect("source ID"));
                let neighbor = u64::from_le_bytes(bytes[8..].try_into().expect("neighbor ID"));
                if source >= self.nodes || neighbor >= self.nodes || source == neighbor {
                    return Err(invalid("invalid global partition edge"));
                }
                visitor(source, neighbor)?;
            }
            Ok(())
        })
    }
}

impl DiskANNBuildInput {
    /// Construct one admitted Vamana graph at a time. All temporary runs share the capture's existing disk allowance, and all buffers retain its original memory/cancellation control.
    pub fn build_partitions(
        &self,
        directory: &Path,
        parameters: DiskANNIndexParams,
        options: DiskANNPartitionOptions,
    ) -> StorageBackendResult<DiskANNPartitionRuns> {
        self.control.check()?;
        let parameters = parameters.validate(self.dimensions)?;
        let options = options.validate()?;
        let writer = RunWriter::new(directory, &self.temporary, &self.control)?;
        let mut builder = Builder {
            input: self,
            directory,
            writer,
            summary: DiskANNPartitionSummary {
                work_order_revision: DiskANNPartitionOptions::WORK_ORDER_REVISION,
                options,
                parameters,
                coverage: self.coverage,
                partitions: 0,
                memberships: 0,
                edges: 0,
                maximum_depth: 0,
                maximum_partition_points: 0,
                assignment_digest: [0; 32],
            },
            hash: provenance(self.coverage, parameters, options),
        };
        builder.partition(Ids::All(self.node_count()), 0, options.coarse_training.seed)?;
        self.control.check()?;
        builder
            .hash
            .update(builder.summary.partitions.to_le_bytes());
        builder
            .hash
            .update(builder.summary.memberships.to_le_bytes());
        builder.summary.assignment_digest = builder.hash.finalize().into();
        Ok(DiskANNPartitionRuns {
            edges: builder.writer.finish()?,
            summary: builder.summary,
            nodes: self.node_count(),
            control: self.control.clone(),
        })
    }
}

struct Builder<'a> {
    input: &'a DiskANNBuildInput,
    directory: &'a Path,
    writer: RunWriter,
    summary: DiskANNPartitionSummary,
    hash: Sha256,
}

impl Builder<'_> {
    fn partition(&mut self, source: Ids<'_>, depth: u8, seed: u64) -> StorageBackendResult<()> {
        self.input.control.check()?;
        let count = source.len();
        if count == 0 {
            return Ok(());
        }
        self.summary.maximum_depth = self.summary.maximum_depth.max(depth);
        let options = self.summary.options;
        if count <= options.max_partition_points as u64 || depth == options.max_depth {
            return self.capacity(source);
        }
        let split = source::split(
            self.input,
            source,
            self.directory,
            options.coarse_training,
            seed,
        )?;
        let reduces = split
            .counts
            .iter()
            .all(|&child| u128::from(child) * 4 <= u128::from(count) * 3);
        self.hash.update([b'S', depth, u8::from(reduces)]);
        self.hash.update(seed.to_le_bytes());
        self.hash.update(split.digest);
        if !reduces {
            drop(split);
            return self.capacity(source);
        }
        for label in 0..split.counts.len() {
            self.partition(
                Ids::Child(&split, label as u8),
                depth + 1,
                source::child_seed(seed, label as u8),
            )?;
        }
        Ok(())
    }

    fn capacity(&mut self, source: Ids<'_>) -> StorageBackendResult<()> {
        leaf::windows(
            source,
            self.summary.options.max_partition_points,
            &self.input.control,
            &mut |ids| self.leaf(ids),
        )
    }
}

fn provenance(
    coverage: DiskANNBuildCoverage,
    parameters: DiskANNIndexParams,
    options: DiskANNPartitionOptions,
) -> Sha256 {
    let mut hash = Sha256::new();
    hash.update(b"UQA DiskANN partitions\0\x01");
    hash.update(coverage.digest());
    for value in [
        parameters.max_degree as u64,
        parameters.build_list_size as u64,
        parameters.search_list_size as u64,
        parameters.alpha.get().to_bits(),
        parameters.beam_width as u64,
        parameters.pq_bytes as u64,
        parameters.seed,
        u64::from(parameters.format_revision),
        u64::from(parameters.algorithm_revision),
        options.max_partition_points as u64,
        u64::from(options.max_depth),
        u64::from(options.coarse_training.max_samples),
        u64::from(options.coarse_training.max_iterations),
        u64::from(options.coarse_training.max_centroids),
        options.coarse_training.seed,
    ] {
        hash.update(value.to_le_bytes());
    }
    hash
}
