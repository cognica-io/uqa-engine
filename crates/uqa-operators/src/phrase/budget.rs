//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One allocation owner shared by phrase analysis, matching and retained results.

use super::{PhraseError, PhraseResult};
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget},
    ordering::sort_by_with_control,
    CancellationToken, Payload, PostingEntry, PostingList, ScoredEntry,
};

pub struct PhraseBudget<'a> {
    cancellation: &'a CancellationToken,
    memory: MemoryBudget,
}

impl<'a> PhraseBudget<'a> {
    pub fn new(limit: usize, cancellation: &'a CancellationToken) -> Self {
        Self {
            cancellation,
            memory: MemoryBudget::new(limit),
        }
    }

    pub fn with_memory(memory: &MemoryBudget, cancellation: &'a CancellationToken) -> Self {
        Self {
            cancellation,
            memory: memory.clone(),
        }
    }

    pub fn memory(&self) -> &MemoryBudget {
        &self.memory
    }
    pub fn cancellation(&self) -> &CancellationToken {
        self.cancellation
    }

    pub fn check_cancelled(&self) -> PhraseResult<()> {
        self.cancellation.check().map_err(PhraseError::Cancelled)
    }

    /// Append a field while both source and destination buffers retain their unique leases.
    pub fn append_results(
        &self,
        output: &mut BudgetedVec<ScoredEntry>,
        rows: Budgeted<Vec<ScoredEntry>>,
    ) -> PhraseResult<()> {
        self.check_cancelled()?;
        output.reserve(rows.len())?;
        for row in rows.iter() {
            self.check_cancelled()?;
            output.push(row.clone())?;
        }
        drop(rows);
        Ok(())
    }

    /// Keep the final posting buffer reserved after source scores and scratch are dropped.
    pub fn finish_postings(
        &self,
        mut rows: BudgetedVec<ScoredEntry>,
    ) -> PhraseResult<Budgeted<PostingList>> {
        sort_by_with_control(
            &mut rows,
            &mut || self.check_cancelled(),
            |left, right, _| Ok(left.doc_id.cmp(&right.doc_id)),
        )?;
        let mut retained = 0;
        for index in 0..rows.len() {
            self.check_cancelled()?;
            if retained > 0 && rows[retained - 1].doc_id == rows[index].doc_id {
                rows[retained - 1].score = rows[retained - 1].score.max(rows[index].score);
            } else {
                rows.swap(retained, index);
                retained += 1;
            }
        }
        rows.truncate(retained);
        let mut entries = BudgetedVec::new(&self.memory);
        entries.reserve(rows.len())?;
        for row in rows.iter() {
            self.check_cancelled()?;
            entries.push(PostingEntry::new(
                row.doc_id,
                Payload {
                    score: row.score,
                    ..Default::default()
                },
            ))?;
        }
        self.check_cancelled()?;
        let (entries, memory) = entries.into_parts();
        Ok(Budgeted::new(
            PostingList::from_sorted_unchecked(entries),
            memory,
        ))
    }
}
