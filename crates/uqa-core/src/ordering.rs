//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible in-place ordering with bounded cancellation checks and no scratch allocation.

use std::cmp::Ordering;

/// Order a mutable slice without scratch allocation, forwarding cancellation and comparison failures.
///
/// The sort is unstable. A failure preserves every element but may leave the order partially changed; retained allocation guards stay attached to their elements. Long element comparisons must check the supplied callback themselves.
pub fn sort_by_with_control<T, E>(
    values: &mut [T],
    poll: &mut dyn FnMut() -> Result<(), E>,
    mut compare: impl FnMut(&T, &T, &mut dyn FnMut() -> Result<(), E>) -> Result<Ordering, E>,
) -> Result<(), E> {
    fn sift<T, E>(
        values: &mut [T],
        mut root: usize,
        poll: &mut dyn FnMut() -> Result<(), E>,
        compare: &mut impl FnMut(&T, &T, &mut dyn FnMut() -> Result<(), E>) -> Result<Ordering, E>,
    ) -> Result<(), E> {
        while root < values.len() / 2 {
            poll()?;
            let mut child = root * 2 + 1;
            if child + 1 < values.len()
                && compare(&values[child], &values[child + 1], poll)?.is_lt()
            {
                child += 1;
            }
            if !compare(&values[root], &values[child], poll)?.is_lt() {
                break;
            }
            values.swap(root, child);
            root = child;
        }
        Ok(())
    }
    poll()?;
    for root in (0..values.len() / 2).rev() {
        sift(values, root, poll, &mut compare)?;
    }
    for end in (1..values.len()).rev() {
        poll()?;
        values.swap(0, end);
        sift(&mut values[..end], 0, poll, &mut compare)?;
    }
    poll()
}

#[cfg(test)]
mod tests;
