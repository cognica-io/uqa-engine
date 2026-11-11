//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible in-place ordering with bounded cancellation checks and no scratch allocation.

use std::{cmp::Ordering, future::Future};

mod heap;
use heap::{HeapOrder, Step};

/// Order a mutable slice without scratch allocation, forwarding cancellation and comparison failures.
///
/// The sort is unstable. A failure preserves every element but may leave the order partially changed; retained allocation guards stay attached to their elements. Long element comparisons must check the supplied callback themselves.
pub fn sort_by_with_control<T, E>(
    values: &mut [T],
    poll: &mut dyn FnMut() -> Result<(), E>,
    mut compare: impl FnMut(&T, &T, &mut dyn FnMut() -> Result<(), E>) -> Result<Ordering, E>,
) -> Result<(), E> {
    let mut cursor = HeapOrder::new(values.len());
    let mut order = Ordering::Equal;
    while let Some(step) = cursor.next(order) {
        match step {
            Step::Checkpoint => poll()?,
            Step::Compare(left, right) => order = compare(&values[left], &values[right], poll)?,
            Step::Swap(left, right) => values.swap(left, right),
        }
    }
    Ok(())
}

/// Owning work for resumable ordering, including suspension within a long comparison.
pub trait SortProgress<T, E> {
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), E>> + Send;
    fn compare(&mut self, left: &T, right: &T) -> impl Future<Output = Result<Ordering, E>> + Send;
}

/// Drive the same allocation-free, unstable ordering through retained progress. A failed comparison or checkpoint preserves the original elements and their allocations, with a possibly incomplete order.
pub async fn sort_by_with_progress<T, E>(
    values: &mut [T],
    progress: &mut impl SortProgress<T, E>,
) -> Result<(), E> {
    let mut cursor = HeapOrder::new(values.len());
    let mut order = Ordering::Equal;
    while let Some(step) = cursor.next(order) {
        match step {
            Step::Checkpoint => progress.checkpoint().await?,
            Step::Compare(left, right) => {
                order = progress.compare(&values[left], &values[right]).await?;
            }
            Step::Swap(left, right) => values.swap(left, right),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
