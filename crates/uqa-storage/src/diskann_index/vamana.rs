//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic Vamana construction within an already admitted navigation partition.

use uqa_core::{memory::BudgetedVec, DocId};

use super::NavigationVector;
use crate::read_control::StorageReadControl;
use crate::vector_index::{DiskANNAlpha, DiskANNIndexParams};
use crate::{StorageBackendError, StorageBackendResult};

mod graph;
mod initialize;
mod memory;
mod prune;
mod search;
pub(in crate::diskann_index) use initialize::select_entry;
pub(in crate::diskann_index) use prune::Selection;
#[cfg(test)]
mod tests;

/// One navigable vector from a caller-owned partition, in strictly increasing document/ordinal order. Canonical values and origin versions remain with their build owner.
#[derive(Clone, Copy)]
pub struct VamanaPoint<'a> {
    pub doc_id: DocId,
    pub ordinal: u32,
    pub vector: &'a NavigationVector,
}

/// Controlled directed adjacency. Dense IDs retain their exact input logical keys; this graph supplies no posting payloads or public scores.
#[derive(Debug)]
pub struct VamanaGraph {
    keys: BudgetedVec<(DocId, u32)>,
    edges: BudgetedVec<u64>,
    lengths: BudgetedVec<usize>,
    degree: usize,
    entry: Option<u64>,
}

impl VamanaGraph {
    /// Build two full-vector refinement passes, then reserve the stable successor cycle. All newly owned graph and work buffers use `control`; borrowed input vectors retain their caller's existing allowance.
    pub fn build(
        dimensions: u32,
        points: &[VamanaPoint<'_>],
        parameters: DiskANNIndexParams,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let parameters = parameters.validate(dimensions)?;
        validate_input(dimensions, points, control)?;
        let mut graph = Self::empty(points, parameters.max_degree, control)?;
        if points.is_empty() {
            return Ok(graph);
        }
        graph.entry = Some(initialize::entry(points, parameters.seed, control)?);
        if points.len() == 1 {
            return Ok(graph);
        }
        initialize::edges(&mut graph, parameters.seed, control)?;
        let order = initialize::permutation(points.len(), parameters.seed, control)?;
        for alpha in [DiskANNAlpha::new(1.0)?, parameters.alpha] {
            graph.refine(points, &order, parameters.build_list_size, alpha, control)?;
        }
        graph.connect(points, parameters.alpha, control)?;
        control.check()?;
        Ok(graph)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn entry(&self) -> Option<u64> {
        self.entry
    }

    pub fn logical_key(&self, node: u64) -> StorageBackendResult<(DocId, u32)> {
        Ok(self.keys[self.position(node)?])
    }

    pub fn neighbors(&self, node: u64) -> StorageBackendResult<&[u64]> {
        let index = self.position(node)?;
        let offset = index * self.degree;
        Ok(&self.edges[offset..offset + self.lengths[index]])
    }

    fn refine(
        &mut self,
        points: &[VamanaPoint<'_>],
        order: &[u64],
        list_size: usize,
        alpha: DiskANNAlpha,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let mut search = search::Workspace::new(self.len(), list_size, self.degree, control)?;
        for &source in order {
            control.check()?;
            let visited = search.visited(self, points, source, control)?;
            let neighbors = prune::select(
                points,
                source,
                visited,
                self.neighbors(source)?,
                alpha,
                self.degree,
                control,
            )?;
            self.replace(source, &neighbors, control)?;
            for &neighbor in &*neighbors {
                control.check()?;
                let outgoing = self.neighbors(neighbor)?;
                if outgoing.contains(&source) {
                    continue;
                }
                if outgoing.len() < self.degree {
                    self.append(neighbor, source, control)?;
                } else {
                    let revised = prune::select(
                        points,
                        neighbor,
                        outgoing,
                        &[source],
                        alpha,
                        self.degree,
                        control,
                    )?;
                    self.replace(neighbor, &revised, control)?;
                }
            }
        }
        Ok(())
    }

    fn connect(
        &mut self,
        points: &[VamanaPoint<'_>],
        alpha: DiskANNAlpha,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        if self.len() < 2 {
            return Ok(());
        }
        for source in 0..self.len() {
            control.check()?;
            let successor = ((source + 1) % self.len()) as u64;
            let mut candidates = BudgetedVec::new(control.memory());
            for &neighbor in self.neighbors(source as u64)? {
                if neighbor != successor {
                    candidates.push(neighbor)?;
                }
            }
            let mut selected = prune::select(
                points,
                source as u64,
                &candidates,
                &[],
                alpha,
                self.degree - 1,
                control,
            )?;
            selected.push(successor)?;
            selected.sort_unstable();
            self.replace(source as u64, &selected, control)?;
        }
        Ok(())
    }
}

fn validate_input(
    dimensions: u32,
    points: &[VamanaPoint<'_>],
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut previous = None;
    for point in points {
        control.check()?;
        let key = (point.doc_id, point.ordinal);
        if previous.is_some_and(|previous| previous >= key) {
            return Err(invalid("logical vector keys must be strictly increasing"));
        }
        if point.vector.coordinates().len() != dimensions as usize {
            return Err(invalid("navigation dimensions differ"));
        }
        previous = Some(key);
    }
    Ok(())
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid Vamana graph: {message}"))
}
