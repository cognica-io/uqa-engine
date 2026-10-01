//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded stable top-K within each memory-bounded sort run.

use super::{
    compare_records, DecoratedRow, EncodedBatchSizer, ExecResult, ExternalSort, Ordering,
    RowSchema, SortKey,
};

impl ExternalSort<'_> {
    /// Return true when the candidate was discarded or replaced the worst retained row. A false result appends normally, flushing the run first if its byte budget requires it.
    pub(super) fn retain_top_candidate(
        &self,
        heap: &mut [DecoratedRow],
        size: &mut EncodedBatchSizer,
        candidate: &DecoratedRow,
    ) -> ExecResult<bool> {
        let Some(keep) = self.keep.filter(|keep| *keep > 0) else {
            return Ok(false);
        };
        if heap.len() < keep {
            return Ok(false);
        }
        if compare_records(
            &self.keys,
            &self.run_schema,
            self.input_slots.len(),
            candidate,
            &heap[0],
        )? != Ordering::Less
        {
            return Ok(true);
        }
        let mut replacement_size = *size;
        replacement_size.remove(&heap[0].row)?;
        replacement_size.append(&candidate.row)?;
        if replacement_size.bytes() > self.work_mem_bytes {
            return Ok(false);
        }
        heap[0] = DecoratedRow {
            row: candidate.row.clone(),
            sequence: candidate.sequence,
        };
        sift_down(
            heap,
            &self.keys,
            &self.run_schema,
            self.input_slots.len(),
            0,
        )?;
        *size = replacement_size;
        Ok(true)
    }
}

pub(super) fn heapify(
    heap: &mut [DecoratedRow],
    keys: &[SortKey],
    schema: &RowSchema,
    width: usize,
) -> ExecResult<()> {
    for parent in (0..heap.len() / 2).rev() {
        sift_down(heap, keys, schema, width, parent)?;
    }
    Ok(())
}

fn sift_down(
    heap: &mut [DecoratedRow],
    keys: &[SortKey],
    schema: &RowSchema,
    width: usize,
    mut parent: usize,
) -> ExecResult<()> {
    loop {
        let left = parent * 2 + 1;
        if left >= heap.len() {
            break;
        }
        let right = left + 1;
        let child = if right < heap.len()
            && compare_records(keys, schema, width, &heap[right], &heap[left])? == Ordering::Greater
        {
            right
        } else {
            left
        };
        if compare_records(keys, schema, width, &heap[child], &heap[parent])? != Ordering::Greater {
            break;
        }
        heap.swap(parent, child);
        parent = child;
    }
    Ok(())
}
