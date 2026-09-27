//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical navigation emits generation-local nodes, before canonical visibility, document projection or scoring.

use std::cmp::Ordering;

use uqa_core::memory::{BudgetedHashSet, BudgetedVec};

use super::{
    format::DiskANNNode,
    pages::{DiskANNPageReadStats, DiskANNReader, PageReadStatsExt},
    NavigationVector, PQDistance, PQLookupTable,
};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

mod refinement;
use refinement::Refinement;

pub use uqa_core::vector_execution::DiskANNTraversalStats;

#[derive(Clone, Copy)]
struct Candidate {
    node: u64,
    distance: PQDistance,
}

impl Candidate {
    fn compare(&self, other: &Self) -> Ordering {
        self.distance
            .get()
            .total_cmp(&other.distance.get())
            .then_with(|| self.node.cmp(&other.node))
    }
}

#[derive(PartialEq, Eq)]
enum Phase {
    Approximate,
    Exhausted,
    Completing,
}

struct Workspace {
    lookup: Option<PQLookupTable>,
    refinement: Option<Refinement>,
    frontier: BudgetedVec<Candidate>,
    expanded: BudgetedHashSet<u64>,
    phase: Phase,
    cursor: u64,
}

/// One retained generation and original query control. Every failed step releases navigation workspace and makes this traversal non-resumable; the caller must discard all previously emitted candidates on error.
pub struct DiskANNTraversal {
    reader: DiskANNReader,
    control: StorageReadControl,
    workspace: Option<Workspace>,
    complete: bool,
    stats: DiskANNTraversalStats,
    pages: DiskANNPageReadStats,
}

impl DiskANNTraversal {
    /// Exact-only queries must use their canonical path. The input type admits only navigable queries; dimensions are checked even for an empty generation.
    pub fn new(
        reader: DiskANNReader,
        query: &NavigationVector,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let input = reader.manifest().input();
        if query.coordinates().len() != input.dimensions as usize {
            return Err(invalid("query dimensions differ from generation"));
        }
        let mut stats = DiskANNTraversalStats::default();
        let workspace = if let Some(entry) = input.entry_node {
            let lookup = reader
                .codebook()
                .ok_or_else(|| invalid("nonempty generation has no codebook"))?
                .lookup(query, control)?;
            let mut frontier = BudgetedVec::new(control.memory());
            let code = reader
                .code(entry)
                .ok_or_else(|| invalid("entry has no resident code"))?;
            frontier.push(Candidate {
                node: entry,
                distance: lookup.estimate(code, control)?,
            })?;
            stats.pq_estimates = 1;
            Some(Workspace {
                lookup: Some(lookup),
                refinement: Some(Refinement::new(
                    query,
                    input.parameters.search_list_size,
                    control,
                )?),
                frontier,
                expanded: BudgetedHashSet::new(control.memory()),
                phase: Phase::Approximate,
                cursor: 0,
            })
        } else {
            None
        };
        control.check()?;
        Ok(Self {
            reader,
            control: control.clone(),
            complete: workspace.is_none(),
            workspace,
            stats,
            pages: DiskANNPageReadStats::default(),
        })
    }

    pub fn stats(&self) -> DiskANNTraversalStats {
        self.stats
    }

    /// Actual page/cache/provider outcomes for this traversal. These remain separate from cache-independent navigation statistics.
    pub fn page_stats(&self) -> DiskANNPageReadStats {
        self.pages
    }

    /// Freeze up to the configured beam width in (PQ distance, node ID) order, read their pages together and expand in that order. An empty batch marks approximate exhaustion, not exact support or a complete document quota.
    pub fn next_beam(&mut self) -> StorageBackendResult<BudgetedVec<DiskANNNode>> {
        self.step(false)
    }

    /// After approximate exhaustion, visit unexpanded generation IDs in ascending order. Callers may stop once their canonical document quota is met. This explicit continuation never substitutes for a failed read or validates canonical visibility.
    pub fn complete_next_beam(&mut self) -> StorageBackendResult<BudgetedVec<DiskANNNode>> {
        self.step(true)
    }

    fn step(&mut self, completion: bool) -> StorageBackendResult<BudgetedVec<DiskANNNode>> {
        // Taking ownership before any fallible work prevents retrying a partially processed beam.
        let workspace = self.workspace.take();
        self.control.check()?;
        let Some(mut workspace) = workspace else {
            return if self.complete {
                Ok(BudgetedVec::new(self.control.memory()))
            } else {
                Err(invalid("traversal failed and cannot resume"))
            };
        };
        let mut stats = self.stats;
        let mut pages = self.pages;
        let nodes = workspace.step(
            &self.reader,
            &self.control,
            &mut stats,
            &mut pages,
            completion,
        )?;
        self.control.check()?;
        self.stats = stats;
        self.pages = pages;
        if completion && workspace.cursor == self.reader.manifest().input().nodes {
            self.complete = true;
        } else {
            self.workspace = Some(workspace);
        }
        Ok(nodes)
    }
}

impl Workspace {
    fn step(
        &mut self,
        reader: &DiskANNReader,
        control: &StorageReadControl,
        stats: &mut DiskANNTraversalStats,
        pages: &mut DiskANNPageReadStats,
        completion: bool,
    ) -> StorageBackendResult<BudgetedVec<DiskANNNode>> {
        let input = reader.manifest().input();
        let width = input.parameters.beam_width;
        let mut selected = BudgetedVec::new(control.memory());
        if completion {
            if self.phase == Phase::Approximate {
                return Err(invalid("completion requires approximate exhaustion"));
            }
            self.phase = Phase::Completing;
            self.lookup = None;
            self.refinement = None;
            self.frontier = BudgetedVec::new(control.memory());
            while self.cursor < input.nodes && selected.len() < width {
                control.check()?;
                let node = self.cursor;
                self.cursor += 1;
                if !self.expanded.contains(&node) {
                    selected.push(node)?;
                }
            }
        } else {
            if self.phase == Phase::Completing {
                return Err(invalid(
                    "approximate traversal cannot resume after completion starts",
                ));
            }
            for candidate in self.frontier.iter() {
                control.check()?;
                if !self.expanded.contains(&candidate.node) {
                    selected.push(candidate.node)?;
                    if selected.len() == width {
                        break;
                    }
                }
            }
            if selected.is_empty() {
                self.phase = Phase::Exhausted;
            }
            self.expanded.reserve(selected.len())?;
        }
        if selected.is_empty() {
            return Ok(BudgetedVec::new(control.memory()));
        }
        let (nodes, read) = reader.read_nodes_with_stats(&selected, control)?;
        pages.merge(read)?;
        if !completion {
            let refinement = self.refinement.as_mut().expect("approximate refinement");
            for node in nodes.iter() {
                control.check()?;
                self.expanded.insert(node.node_id())?;
                refinement.offer(node, control)?;
            }
            let mut retained = 0;
            for offset in 0..self.frontier.len() {
                super::metric::checkpoint(offset, control)?;
                let candidate = self.frontier[offset];
                if !self.expanded.contains(&candidate.node) && refinement.allows(&candidate) {
                    self.frontier[retained] = candidate;
                    retained += 1;
                }
            }
            self.frontier.truncate(retained);
        }
        for node in nodes.iter() {
            control.check()?;
            if completion {
                increment(&mut stats.completion_expansions)?;
            } else {
                increment(&mut stats.approximate_expansions)?;
                for &neighbor in node.neighbors() {
                    control.check()?;
                    self.admit(neighbor, reader, control, stats)?;
                }
            }
        }
        increment(&mut stats.beams)?;
        Ok(nodes)
    }

    fn admit(
        &mut self,
        node: u64,
        reader: &DiskANNReader,
        control: &StorageReadControl,
        stats: &mut DiskANNTraversalStats,
    ) -> StorageBackendResult<()> {
        if self.expanded.contains(&node) {
            return Ok(());
        }
        for (offset, item) in self.frontier.iter().enumerate() {
            super::metric::checkpoint(offset, control)?;
            if item.node == node {
                return Ok(());
            }
        }
        let code = reader
            .code(node)
            .ok_or_else(|| invalid("neighbor has no resident code"))?;
        let candidate = Candidate {
            node,
            distance: self
                .lookup
                .as_ref()
                .ok_or_else(|| invalid("approximate traversal has no PQ lookup"))?
                .estimate(code, control)?,
        };
        increment(&mut stats.pq_estimates)?;
        if !self
            .refinement
            .as_ref()
            .ok_or_else(|| invalid("approximate traversal has no refinement"))?
            .allows(&candidate)
        {
            return Ok(());
        }
        let limit = reader.manifest().input().parameters.search_list_size;
        if self.frontier.len() < limit {
            self.frontier.push(candidate)?;
        } else if candidate
            .compare(self.frontier.last().expect("positive list limit"))
            .is_lt()
        {
            self.frontier[limit - 1] = candidate;
        } else {
            return Ok(());
        }
        // Only unexpanded work occupies the bounded PQ frontier. A previously evicted candidate may be admitted again if a later edge rediscovers it.
        self.frontier.sort_unstable_by(Candidate::compare);
        Ok(())
    }
}

fn increment(value: &mut u64) -> StorageBackendResult<()> {
    *value = value
        .checked_add(1)
        .ok_or_else(|| invalid("work count overflow"))?;
    Ok(())
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid DiskANN traversal: {message}"))
}

#[cfg(test)]
mod tests;
