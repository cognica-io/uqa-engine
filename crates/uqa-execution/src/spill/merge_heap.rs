//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Run merging and bounded top-K share a heap whose SQL comparisons can fail.

use std::cmp::Ordering;

pub(crate) fn push<T, E>(
    heap: &mut Vec<T>,
    item: T,
    mut compare: impl FnMut(&T, &T) -> Result<Ordering, E>,
) -> Result<(), E> {
    heap.push(item);
    let mut child = heap.len() - 1;
    while child > 0 {
        let parent = (child - 1) / 2;
        if !compare(&heap[child], &heap[parent])?.is_lt() {
            break;
        }
        heap.swap(child, parent);
        child = parent;
    }
    Ok(())
}

pub(crate) fn pop<T, E>(
    heap: &mut Vec<T>,
    compare: impl FnMut(&T, &T) -> Result<Ordering, E>,
) -> Result<Option<T>, E> {
    if heap.is_empty() {
        return Ok(None);
    }
    let first = heap.swap_remove(0);
    sift_down(heap, 0, compare)?;
    Ok(Some(first))
}

pub(crate) fn sift_down<T, E>(
    heap: &mut [T],
    mut parent: usize,
    mut compare: impl FnMut(&T, &T) -> Result<Ordering, E>,
) -> Result<(), E> {
    while parent < heap.len() / 2 {
        let left = parent * 2 + 1;
        let right = left + 1;
        let child = if right < heap.len() && compare(&heap[right], &heap[left])?.is_lt() {
            right
        } else {
            left
        };
        if !compare(&heap[child], &heap[parent])?.is_lt() {
            break;
        }
        heap.swap(parent, child);
        parent = child;
    }
    Ok(())
}
