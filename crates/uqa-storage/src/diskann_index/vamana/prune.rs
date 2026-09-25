//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use super::{invalid, VamanaPoint};
use crate::read_control::StorageReadControl;
use crate::vector_index::DiskANNAlpha;
use crate::StorageBackendResult;

pub(super) fn select(
    points: &[VamanaPoint<'_>],
    source: u64,
    candidates: &[u64],
    existing: &[u64],
    alpha: DiskANNAlpha,
    degree: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u64>> {
    control.check()?;
    let source = position(points, source)?;
    let mut ids = BudgetedVec::new(control.memory());
    for &node in candidates.iter().chain(existing) {
        control.check()?;
        position(points, node)?;
        if node != source as u64 {
            ids.push(node)?;
        }
    }
    ids.sort_unstable();
    let mut pool = BudgetedVec::new(control.memory());
    let mut previous = None;
    for &node in &*ids {
        control.check()?;
        if previous != Some(node) {
            let distance = points[source]
                .vector
                .squared_distance(points[node as usize].vector, control)?
                .get();
            pool.push((node, distance))?;
            previous = Some(node);
        }
    }
    drop(ids);
    pool.sort_unstable_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut selected = BudgetedVec::new(control.memory());
    selected.reserve(degree.min(pool.len()))?;
    // In source-distance order, skipping candidates dominated by an already selected neighbor matches removing them after each selection.
    for &(candidate, distance) in &*pool {
        control.check()?;
        if selected.len() == degree {
            break;
        }
        let mut removed = false;
        for &neighbor in &*selected {
            let between = points[neighbor as usize]
                .vector
                .squared_distance(points[candidate as usize].vector, control)?
                .get();
            if alpha.squared() * between <= distance {
                removed = true;
                break;
            }
        }
        if !removed {
            selected.push(candidate)?;
        }
    }
    Ok(selected)
}

pub(super) fn position(points: &[VamanaPoint<'_>], node: u64) -> StorageBackendResult<usize> {
    usize::try_from(node)
        .ok()
        .filter(|&node| node < points.len())
        .ok_or_else(|| invalid("candidate ID is out of range"))
}
