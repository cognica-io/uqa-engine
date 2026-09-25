//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use std::path::Path;
use uqa_core::memory::BudgetedVec;

use super::runs::{RunReader, RunWriter};
use super::temporary::TemporaryRun;
use super::{invalid, DiskANNBuildInput, DiskANNPartitionRuns, DiskANNPartitionSummary};
use crate::diskann_index::{vamana::Selection, NavigationInput, NavigationVector};
use crate::{read_control::StorageReadControl, StorageBackendResult};

mod sort;
#[cfg(test)]
mod tests;

/// An explicit record capacity for initial sorted chunks. Every merge pass uses two readers; the configured capacity is never silently reduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNMergeOptions {
    pub sort_buffer_records: usize,
}

/// Global adjacency construction provenance, without physical artifact sealing or SQL publication authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNMergeSummary {
    pub work_order_revision: u32,
    pub partitions: DiskANNPartitionSummary,
    pub options: DiskANNMergeOptions,
    pub merge_passes: u8,
    pub edges: u64,
    pub adjacency_digest: [u8; 32],
}

/// Encrypted fixed-width adjacency, with unique sorted neighbors and a reserved global successor for every non-singleton node.
pub struct DiskANNMergedGraph {
    run: TemporaryRun,
    nodes: u64,
    degree: usize,
    summary: DiskANNMergeSummary,
    control: StorageReadControl,
}

impl DiskANNMergedGraph {
    pub(super) fn check_input(&self, input: &DiskANNBuildInput) -> StorageBackendResult<()> {
        input.control.check()?;
        if self.nodes != input.node_count()
            || self.summary.partitions.coverage != input.coverage
            || !self.control.shares_context(&input.control)
            || !self.run.uses_allowance(&input.temporary)
        {
            return Err(invalid("merged graph source or build allowance differs"));
        }
        Ok(())
    }

    pub fn summary(&self) -> &DiskANNMergeSummary {
        &self.summary
    }

    /// Visit every dense node in order, including empty/singleton neighborhoods. Buffers and errors retain the original build control.
    pub fn visit_neighbors(
        &self,
        visitor: &mut dyn FnMut(u64, &[u64]) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.run.read(&self.control, |file| {
            let mut hash = adjacency_hash(&self.summary.partitions, self.nodes, self.degree);
            let mut edges = 0_u64;
            if self.nodes != 0 {
                let mut reader = RunReader::new(file, &self.control)?;
                let mut neighbors = BudgetedVec::new(self.control.memory());
                neighbors.reserve(self.degree)?;
                for node in 0..self.nodes {
                    self.control.check()?;
                    let bytes = reader.record::<8>()?;
                    hash.update(bytes);
                    let count = u64::from_le_bytes(bytes);
                    if count > self.degree as u64 || (self.nodes > 1 && count == 0) {
                        return Err(invalid("invalid merged neighborhood length"));
                    }
                    neighbors.clear();
                    for slot in 0..self.degree {
                        let bytes = reader.record::<8>()?;
                        hash.update(bytes);
                        let neighbor = u64::from_le_bytes(bytes);
                        if (slot as u64) < count {
                            if neighbor >= self.nodes
                                || neighbor == node
                                || neighbors.last().is_some_and(|&last| last >= neighbor)
                            {
                                return Err(invalid("invalid merged neighbor"));
                            }
                            neighbors.push(neighbor)?;
                        } else if neighbor != 0 {
                            return Err(invalid("nonzero merged neighborhood padding"));
                        }
                    }
                    if self.nodes > 1 && !neighbors.contains(&((node + 1) % self.nodes)) {
                        return Err(invalid("merged neighborhood lacks global successor"));
                    }
                    edges = edges
                        .checked_add(count)
                        .ok_or_else(|| invalid("merged edge overflow"))?;
                    visitor(node, &neighbors)?;
                }
            }
            if edges != self.summary.edges
                || <[u8; 32]>::from(hash.finalize()) != self.summary.adjacency_digest
            {
                return Err(invalid(
                    "merged adjacency differs from its construction digest",
                ));
            }
            Ok(())
        })
    }
}

impl DiskANNBuildInput {
    /// Consume partition candidates, externally order them, and prune using full navigation vectors. Capture and candidates must share source coverage and both resource allowances.
    pub fn merge_partitions(
        &self,
        partitions: DiskANNPartitionRuns,
        directory: &Path,
        options: DiskANNMergeOptions,
    ) -> StorageBackendResult<DiskANNMergedGraph> {
        self.control.check()?;
        if options.sort_buffer_records == 0 {
            return Err(invalid("global sort capacity must be positive"));
        }
        let (run, summary) = partitions.into_source(self)?;
        merge(self, run, &summary, directory, options)
    }
}

fn merge(
    input: &DiskANNBuildInput,
    candidates: TemporaryRun,
    partitions: &DiskANNPartitionSummary,
    directory: &Path,
    options: DiskANNMergeOptions,
) -> StorageBackendResult<DiskANNMergedGraph> {
    let sorted = sort::sort(
        input,
        candidates,
        partitions.edges,
        directory,
        options.sort_buffer_records,
    )?;
    let control = &input.control;
    let nodes = input.node_count();
    let degree = (partitions.parameters.max_degree as u64).min(nodes.saturating_sub(1)) as usize;
    let mut writer = RunWriter::new(directory, &input.temporary, control)?;
    if nodes != 0 {
        writer.prepare()?;
    }
    let mut hash = adjacency_hash(partitions, nodes, degree);
    let mut edges = 0_u64;
    sorted.run.read(control, |file| {
        if nodes == 0 {
            return Ok(());
        }
        let mut candidates = sort::Range::new(file, 0, sorted.count, nodes, control)?;
        let mut head = candidates.next()?;
        for node in 0..nodes {
            control.check()?;
            let successor = (node + 1) % nodes;
            let mut selected = Selection::new(
                partitions.parameters.alpha,
                degree.saturating_sub(1),
                control,
            )?;
            let mut vectors = BudgetedVec::new(control.memory());
            vectors.reserve(degree.saturating_sub(1))?;
            let mut previous = None;
            while let Some(candidate) = head.filter(|candidate| candidate.source == node) {
                control.check()?;
                if previous != Some(candidate.neighbor)
                    && candidate.neighbor != successor
                    && !selected.is_full()
                {
                    let vector = navigation(input, candidate.neighbor)?;
                    if selected.consider(
                        candidate.neighbor,
                        candidate.distance,
                        control,
                        |position, _| {
                            let neighbor: &NavigationVector = &vectors[position];
                            neighbor.squared_distance(&vector, control)
                        },
                    )? {
                        vectors.push(vector)?;
                    }
                }
                previous = Some(candidate.neighbor);
                head = candidates.next()?;
            }
            drop(vectors);
            let mut neighbors = selected.finish();
            if nodes > 1 {
                neighbors.push(successor)?;
            }
            neighbors.sort_unstable();
            edges = edges
                .checked_add(neighbors.len() as u64)
                .ok_or_else(|| invalid("merged edge overflow"))?;
            write_word(neighbors.len() as u64, &mut writer, &mut hash)?;
            for slot in 0..degree {
                control.check()?;
                write_word(
                    neighbors.get(slot).copied().unwrap_or(0),
                    &mut writer,
                    &mut hash,
                )?;
            }
        }
        if head.is_some() {
            return Err(invalid("global candidate source was not consumed"));
        }
        Ok(())
    })?;
    let summary = DiskANNMergeSummary {
        work_order_revision: 1,
        partitions: *partitions,
        options,
        merge_passes: sorted.passes,
        edges,
        adjacency_digest: hash.finalize().into(),
    };
    drop(sorted);
    let run = writer.finish()?;
    control.check()?;
    Ok(DiskANNMergedGraph {
        run,
        nodes,
        degree,
        summary,
        control: control.clone(),
    })
}

fn navigation(input: &DiskANNBuildInput, node: u64) -> StorageBackendResult<NavigationVector> {
    let raw = input.read_node(node)?;
    match NavigationInput::from_raw(input.dimensions, raw.raw(), &input.control)? {
        NavigationInput::Navigable(vector) => Ok(vector),
        NavigationInput::Exact(_) => Err(invalid(
            "global candidate changed navigation classification",
        )),
    }
}

fn adjacency_hash(partitions: &DiskANNPartitionSummary, nodes: u64, degree: usize) -> Sha256 {
    crate::diskann_index::format::adjacency_hash(partitions.coverage, nodes, degree)
}

fn write_word(value: u64, writer: &mut RunWriter, hash: &mut Sha256) -> StorageBackendResult<()> {
    let bytes = value.to_le_bytes();
    writer.append(&bytes)?;
    hash.update(bytes);
    Ok(())
}
