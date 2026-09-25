//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned work order uses independent deterministic streams for edges, permutation and entry sampling.

use uqa_core::memory::BudgetedVec;

use super::{invalid, VamanaGraph, VamanaPoint};
use crate::diskann_index::{metric, random::SplitMix64};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const ORDER_STREAM: u64 = 0x9e37_79b9_7f4a_7c15;
const ENTRY_STREAM: u64 = 0x94d0_49bb_1331_11eb;
const ENTRY_SAMPLE: usize = 256;

/// Floyd sampling avoids clearing an entire population array for each small outgoing neighborhood.
fn sample(
    population: usize,
    count: usize,
    random: &mut SplitMix64,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u64>> {
    if count > population {
        return Err(invalid("sample exceeds population"));
    }
    let mut chosen = BudgetedVec::new(control.memory());
    chosen.reserve(count)?;
    for upper in population - count..population {
        control.check()?;
        let candidate = random.below(upper as u64 + 1, control)?;
        chosen.push(if chosen.contains(&candidate) {
            upper as u64
        } else {
            candidate
        })?;
    }
    chosen.sort_unstable();
    Ok(chosen)
}

pub(super) fn edges(
    graph: &mut VamanaGraph,
    seed: u64,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    if graph.len() < 2 {
        return Ok(());
    }
    let mut random = SplitMix64(seed);
    for node in 0..graph.len() {
        control.check()?;
        let mut neighbors = sample(graph.len() - 1, graph.degree, &mut random, control)?;
        for neighbor in &mut *neighbors {
            if *neighbor >= node as u64 {
                *neighbor += 1;
            }
        }
        graph.replace(node as u64, &neighbors, control)?;
    }
    Ok(())
}

pub(super) fn permutation(
    count: usize,
    seed: u64,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u64>> {
    let mut order = BudgetedVec::new(control.memory());
    order.reserve(count)?;
    for index in 0..count {
        metric::checkpoint(index, control)?;
        order.push(index as u64)?;
    }
    let mut random = SplitMix64(seed ^ ORDER_STREAM);
    for index in 0..count {
        let selected = index + random.below((count - index) as u64, control)? as usize;
        order.swap(index, selected);
    }
    Ok(order)
}

pub(super) fn entry(
    points: &[VamanaPoint<'_>],
    seed: u64,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let dimensions = points
        .first()
        .ok_or_else(|| invalid("entry requires a point"))?
        .vector
        .coordinates()
        .len();
    let sample = entry_sample(points.len(), seed, control)?;
    let mut centroid = BudgetedVec::new(control.memory());
    centroid.reserve(dimensions)?;
    for coordinate in 0..dimensions {
        metric::checkpoint(coordinate, control)?;
        let mut sum = 0.0;
        for &node in &*sample {
            sum += points[node as usize].vector.coordinates()[coordinate];
        }
        centroid.push(sum / sample.len() as f64)?;
    }
    let mut best = (0, f64::INFINITY);
    for (index, point) in points.iter().enumerate() {
        control.check()?;
        let distance = metric::squared_distance(point.vector.coordinates(), &centroid, control)?;
        if distance < best.1 {
            best = (index as u64, distance);
        }
    }
    Ok(best.0)
}

pub(super) fn entry_sample(
    count: usize,
    seed: u64,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u64>> {
    sample(
        count,
        count.min(ENTRY_SAMPLE),
        &mut SplitMix64(seed ^ ENTRY_STREAM),
        control,
    )
}
