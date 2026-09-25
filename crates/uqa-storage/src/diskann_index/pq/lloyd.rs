//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ops::Range;

use uqa_core::memory::BudgetedVec;

use super::{checkpoint, filled, nearest};
use crate::{read_control::StorageReadControl, StorageBackendResult};

pub(super) fn train(
    samples: &[BudgetedVec<f64>],
    range: Range<usize>,
    centroids: &mut [f64],
    iterations: u32,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let width = range.len();
    let count = centroids.len() / width;
    let mut labels = filled(samples.len(), u16::MAX, control)?;
    let mut sums = filled(centroids.len(), 0.0_f64, control)?;
    let mut counts = filled(count, 0_u32, control)?;
    for _ in 0..iterations {
        control.check()?;
        for (offset, sum) in sums.iter_mut().enumerate() {
            checkpoint(offset, control)?;
            *sum = 0.0;
        }
        counts.fill(0);
        let mut changed = false;
        for (index, sample) in samples.iter().enumerate() {
            control.check()?;
            let coordinates = &sample[range.clone()];
            let label = nearest(coordinates, centroids, control)?;
            changed |= labels[index] != u16::from(label);
            labels[index] = u16::from(label);
            counts[usize::from(label)] += 1;
            let start = usize::from(label) * width;
            for (offset, (&value, sum)) in coordinates
                .iter()
                .zip(&mut sums[start..start + width])
                .enumerate()
            {
                checkpoint(offset, control)?;
                *sum += value;
            }
        }
        if !changed {
            break;
        }
        for (cluster, centroid) in centroids.chunks_exact_mut(width).enumerate() {
            control.check()?;
            if counts[cluster] == 0 {
                continue;
            }
            let start = cluster * width;
            for (offset, (value, &sum)) in centroid
                .iter_mut()
                .zip(&sums[start..start + width])
                .enumerate()
            {
                checkpoint(offset, control)?;
                *value = sum / f64::from(counts[cluster]);
            }
        }
    }
    control.check()
}
