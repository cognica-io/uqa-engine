//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::mem::size_of;
use uqa_core::memory::MemoryError;

use super::VamanaGraph;
use crate::{vector_index::DiskANNIndexParams, StorageBackendResult};

impl VamanaGraph {
    /// Conservative requested-buffer bound, excluding borrowed points. Dynamic vectors include simultaneous old/new growth; actual controlled reservations remain authoritative.
    pub(in crate::diskann_index) fn allocation_bound(
        dimensions: u32,
        count: usize,
        parameters: DiskANNIndexParams,
    ) -> StorageBackendResult<usize> {
        parameters.validate(dimensions)?;
        if count == 0 {
            return Ok(0);
        }
        let degree = parameters.max_degree.min(count - 1);
        let frontier = parameters.build_list_size.saturating_add(degree).min(count);
        let mut total = 0usize;
        // Owned graph, order, bitmap, visited replacement, frontier, prune ID/pool replacements, retained neighbors and entry work.
        for (entries, copies, width) in [
            (count, 1, size_of::<(u64, u32)>()),
            (count, 1, size_of::<usize>()),
            (count, degree, size_of::<u64>()),
            (count, 1, size_of::<u64>()),
            (count, 1, size_of::<bool>()),
            (count, 3, size_of::<u64>()),
            (frontier, 1, size_of::<(u64, f64)>()),
            (
                count.checked_add(degree).ok_or(MemoryError::SizeOverflow)?,
                3,
                size_of::<u64>(),
            ),
            (count, 3, size_of::<(u64, f64)>()),
            (degree, 4, size_of::<u64>()),
            (count.min(256), 1, size_of::<u64>()),
            (dimensions as usize, 1, size_of::<f64>()),
        ] {
            total = entries
                .checked_mul(copies)
                .and_then(|n| n.checked_mul(width))
                .and_then(|bytes| total.checked_add(bytes))
                .ok_or(MemoryError::SizeOverflow)?;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diskann_index::{NavigationInput, NavigationVector, VamanaPoint};
    use crate::read_control::StorageReadControl;

    #[test]
    fn requested_graph_bound_covers_live_growth_across_degree_boundaries() {
        let input = StorageReadControl::with_limit(1 << 20);
        for count in [0, 1, 2, 17, 65] {
            let vectors: Vec<NavigationVector> = (0..count)
                .map(|i| {
                    let NavigationInput::Navigable(vector) =
                        NavigationInput::from_raw(2, &[1.0, i as f32], &input).unwrap()
                    else {
                        unreachable!()
                    };
                    vector
                })
                .collect();
            let points: Vec<_> = vectors
                .iter()
                .enumerate()
                .map(|(i, vector)| VamanaPoint {
                    doc_id: i as u64,
                    ordinal: 0,
                    vector,
                })
                .collect();
            for degree in [2, 4, 64] {
                let mut parameters = DiskANNIndexParams::for_dimensions(2).unwrap();
                parameters.max_degree = degree;
                let bound = VamanaGraph::allocation_bound(2, count, parameters).unwrap();
                let control = StorageReadControl::with_limit(bound);
                let graph = VamanaGraph::build(2, &points, parameters, &control).unwrap();
                assert!(control.memory().peak() <= bound);
                drop(graph);
                assert_eq!(control.memory().used(), 0);
            }
        }
    }
}
