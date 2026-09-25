//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::Digest;
use std::mem::size_of;
use uqa_core::memory::{BudgetedVec, MemoryError};

use super::{invalid, Builder, Ids};
use crate::diskann_index::{NavigationInput, NavigationVector, VamanaGraph, VamanaPoint};
use crate::{read_control::StorageReadControl, StorageBackendResult};

struct LoadedPoint {
    global: u64,
    doc: u64,
    ordinal: u32,
    vector: NavigationVector,
}

pub(super) fn windows(
    source: Ids<'_>,
    capacity: usize,
    control: &StorageReadControl,
    visitor: &mut dyn FnMut(&[u64]) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    if capacity < 2 {
        return Err(invalid("partition capacity must be at least two"));
    }
    let mut ids = BudgetedVec::new(control.memory());
    ids.reserve(
        usize::try_from(source.len().min(capacity as u64))
            .map_err(|_| MemoryError::SizeOverflow)?,
    )?;
    let mut emitted = false;
    source.visit(control, &mut |id| {
        ids.push(id)?;
        if ids.len() == capacity {
            visitor(&ids)?;
            ids.clear();
            ids.push(id)?;
            emitted = true;
        }
        Ok(())
    })?;
    if !ids.is_empty() && (!emitted || ids.len() > 1) {
        visitor(&ids)?;
    }
    control.check()
}

impl Builder<'_> {
    pub(super) fn leaf(&mut self, ids: &[u64]) -> StorageBackendResult<()> {
        if ids.len() > 1 {
            self.writer.prepare()?;
        }
        let control = &self.input.control;
        control.check()?;
        let dimensions = self.input.dimensions as usize;
        let count = ids.len();
        let graph =
            VamanaGraph::allocation_bound(self.input.dimensions, count, self.summary.parameters)?;
        let mut additional = graph;
        // Loaded owners and borrowed references coexist with the graph. Decode/normalization has at most three scalar-vector widths plus its fixed record header.
        for (entries, width) in [
            (count, size_of::<LoadedPoint>()),
            (count, size_of::<VamanaPoint<'_>>()),
            (
                count
                    .checked_mul(dimensions)
                    .ok_or(MemoryError::SizeOverflow)?,
                size_of::<f64>(),
            ),
            (dimensions, 3 * size_of::<f32>()),
            (44, 1),
        ] {
            additional = entries
                .checked_mul(width)
                .and_then(|bytes| additional.checked_add(bytes))
                .ok_or(MemoryError::SizeOverflow)?;
        }
        let required = control
            .memory()
            .used()
            .checked_add(additional)
            .ok_or(MemoryError::SizeOverflow)?;
        if required > control.memory().limit() {
            return Err(MemoryError::Limit {
                required,
                limit: control.memory().limit(),
            }
            .into());
        }
        let mut loaded = BudgetedVec::new(control.memory());
        loaded.reserve(count)?;
        for &id in ids {
            let record = self.input.read_node(id)?;
            let NavigationInput::Navigable(vector) =
                NavigationInput::from_raw(self.input.dimensions, record.raw(), control)?
            else {
                return Err(invalid("leaf vector changed classification"));
            };
            loaded.push(LoadedPoint {
                global: id,
                doc: record.doc_id(),
                ordinal: record.ordinal(),
                vector,
            })?;
        }
        let mut points = BudgetedVec::new(control.memory());
        points.reserve(count)?;
        for point in &*loaded {
            control.check()?;
            points.push(VamanaPoint {
                doc_id: point.doc,
                ordinal: point.ordinal,
                vector: &point.vector,
            })?;
        }
        let graph = VamanaGraph::build(
            self.input.dimensions,
            &points,
            self.summary.parameters,
            control,
        )?;
        self.hash.update([b'L']);
        self.hash.update((count as u64).to_le_bytes());
        for (source, point) in loaded.iter().enumerate() {
            self.hash.update(point.global.to_le_bytes());
            for &neighbor in graph.neighbors(source as u64)? {
                let mut bytes = [0; 16];
                bytes[..8].copy_from_slice(&point.global.to_le_bytes());
                bytes[8..].copy_from_slice(&loaded[neighbor as usize].global.to_le_bytes());
                self.writer.append(&bytes)?;
                self.summary.edges = self
                    .summary
                    .edges
                    .checked_add(1)
                    .ok_or_else(|| invalid("partition edge count overflow"))?;
            }
        }
        self.summary.partitions = self
            .summary
            .partitions
            .checked_add(1)
            .ok_or_else(|| invalid("partition count overflow"))?;
        self.summary.memberships = self
            .summary
            .memberships
            .checked_add(count as u64)
            .ok_or_else(|| invalid("partition membership count overflow"))?;
        self.summary.maximum_partition_points = self.summary.maximum_partition_points.max(count);
        control.check()
    }
}
