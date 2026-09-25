//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned work order uses independent deterministic streams for edges, permutation and entry sampling.

use uqa_core::memory::BudgetedVec;

use super::{invalid, VamanaGraph, VamanaPoint};
use crate::diskann_index::{metric, random::SplitMix64, NavigationVector};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const ORDER_STREAM: u64 = 0x9e37_79b9_7f4a_7c15;
const ENTRY_STREAM: u64 = 0x94d0_49bb_1331_11eb;
const ENTRY_SAMPLE: usize = 256;

type EntryVisitor<'a> = dyn FnMut(&NavigationVector) -> StorageBackendResult<()> + 'a;
type EntrySource<'a> = dyn FnMut(u64, &mut EntryVisitor<'_>) -> StorageBackendResult<()> + 'a;

/// Floyd sampling avoids clearing an entire population array for each small outgoing neighborhood.
fn sample(
    population: u64,
    count: usize,
    random: &mut SplitMix64,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u64>> {
    if count as u64 > population {
        return Err(invalid("sample exceeds population"));
    }
    let mut chosen = BudgetedVec::new(control.memory());
    chosen.reserve(count)?;
    for upper in population - count as u64..population {
        control.check()?;
        let candidate = random.below(upper + 1, control)?;
        chosen.push(if chosen.contains(&candidate) {
            upper
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
        let mut neighbors = sample((graph.len() - 1) as u64, graph.degree, &mut random, control)?;
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
    select_entry(
        points.len() as u64,
        dimensions,
        seed,
        control,
        &mut |node, visitor| visitor(points[node as usize].vector),
    )
}

/// Capped sample-centroid entry with the same work order for borrowed partitions and streamed global vectors. Each source call must visit one navigation vector.
pub(in crate::diskann_index) fn select_entry(
    count: u64,
    dimensions: usize,
    seed: u64,
    control: &StorageReadControl,
    read: &mut EntrySource<'_>,
) -> StorageBackendResult<u64> {
    control.check()?;
    if count == 0 || dimensions == 0 {
        return Err(invalid("entry requires navigation points"));
    }
    let sample = sample(
        count,
        count.min(ENTRY_SAMPLE as u64) as usize,
        &mut SplitMix64(seed ^ ENTRY_STREAM),
        control,
    )?;
    let mut centroid = BudgetedVec::new(control.memory());
    centroid.reserve(dimensions)?;
    for coordinate in 0..dimensions {
        metric::checkpoint(coordinate, control)?;
        centroid.push(0.0)?;
    }
    for &node in &*sample {
        visit(read, node, dimensions, &mut |vector| {
            for (coordinate, value) in vector.coordinates().iter().enumerate() {
                metric::checkpoint(coordinate, control)?;
                centroid[coordinate] += value;
            }
            Ok(())
        })?;
    }
    for (coordinate, value) in centroid.iter_mut().enumerate() {
        metric::checkpoint(coordinate, control)?;
        *value /= sample.len() as f64;
    }
    let mut best = (0, f64::INFINITY);
    for node in 0..count {
        control.check()?;
        visit(read, node, dimensions, &mut |vector| {
            let distance = metric::squared_distance(vector.coordinates(), &centroid, control)?;
            if distance < best.1 {
                best = (node, distance);
            }
            Ok(())
        })?;
    }
    control.check()?;
    Ok(best.0)
}

#[cfg(test)]
pub(super) fn entry_sample(
    count: usize,
    seed: u64,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u64>> {
    sample(
        count as u64,
        count.min(ENTRY_SAMPLE),
        &mut SplitMix64(seed ^ ENTRY_STREAM),
        control,
    )
}

fn visit(
    read: &mut EntrySource<'_>,
    node: u64,
    dimensions: usize,
    consumer: &mut EntryVisitor<'_>,
) -> StorageBackendResult<()> {
    let mut seen = false;
    let mut failure = None;
    let result = read(node, &mut |vector| {
        if failure.is_some() {
            return Err(invalid("entry source already failed"));
        }
        let outcome = if seen || vector.coordinates().len() != dimensions {
            Err(invalid("entry source identity or dimensions differ"))
        } else {
            seen = true;
            consumer(vector)
        };
        if let Err(error) = outcome {
            failure = Some(error);
            return Err(invalid("entry source rejected"));
        }
        Ok(())
    });
    if let Some(error) = failure {
        return Err(error);
    }
    result?;
    if !seen {
        return Err(invalid("entry source omitted its vector"));
    }
    Ok(())
}
