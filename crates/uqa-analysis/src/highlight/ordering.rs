//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible in-place ordering with bounded cancellation checks and no scratch allocation.

use crate::AnalysisResult;
use std::cmp::Ordering;

pub(super) fn sort_by<T>(
    values: &mut [T],
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
    mut compare: impl FnMut(&T, &T, &mut dyn FnMut() -> AnalysisResult<()>) -> AnalysisResult<Ordering>,
) -> AnalysisResult<()> {
    fn sift<T>(
        values: &mut [T],
        mut root: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
        compare: &mut impl FnMut(
            &T,
            &T,
            &mut dyn FnMut() -> AnalysisResult<()>,
        ) -> AnalysisResult<Ordering>,
    ) -> AnalysisResult<()> {
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
