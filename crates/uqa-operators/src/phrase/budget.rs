//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory reservations and cancellation for one graph-phrase execution.

use super::{PhraseError, PhraseResult};
use uqa_core::{CancellationToken, Payload, PostingEntry, PostingList, ScoredEntry};

/// Query-owned memory allowance shared by query preparation, candidate matching, and result accumulation.
pub struct PhraseBudget<'a> {
    cancellation: &'a CancellationToken,
    limit: usize,
    used: usize,
}

impl<'a> PhraseBudget<'a> {
    pub fn new(limit: usize, cancellation: &'a CancellationToken) -> Self {
        Self {
            cancellation,
            limit,
            used: 0,
        }
    }

    pub fn check_cancelled(&self) -> PhraseResult<()> {
        self.cancellation.check().map_err(PhraseError::Cancelled)
    }

    pub fn reserve_bytes(&mut self, bytes: usize) -> PhraseResult<()> {
        self.check_cancelled()?;
        let required = self
            .used
            .checked_add(bytes)
            .ok_or(PhraseError::MemoryLimit {
                required: usize::MAX,
                limit: self.limit,
            })?;
        if required > self.limit {
            return Err(PhraseError::MemoryLimit {
                required,
                limit: self.limit,
            });
        }
        self.used = required;
        Ok(())
    }

    pub(super) fn used_bytes(&self) -> usize {
        self.used
    }

    pub(super) fn finish_scope(&mut self, before: usize, retained: usize) {
        self.used = before + retained;
    }

    /// Append a field's reserved output while accounting for both vectors during reallocation.
    pub fn append_results(
        &mut self,
        output: &mut Vec<ScoredEntry>,
        rows: Vec<ScoredEntry>,
    ) -> PhraseResult<()> {
        let capacity = rows.capacity();
        for row in rows {
            self.grow(output)?;
            output.push(row);
        }
        self.release_bytes(capacity * size_of::<ScoredEntry>());
        Ok(())
    }

    /// Convert accepted rows to the posting carrier, including the simultaneous source allocation.
    pub fn finish_postings(&mut self, mut rows: Vec<ScoredEntry>) -> PhraseResult<PostingList> {
        self.check_cancelled()?;
        rows.sort_unstable_by_key(|row| row.doc_id);
        rows.dedup_by(|right, left| {
            if left.doc_id != right.doc_id {
                return false;
            }
            left.score = left.score.max(right.score);
            true
        });
        self.reserve_items::<PostingEntry>(rows.len())?;
        let capacity = rows.capacity();
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(rows.len())
            .map_err(PhraseError::Allocation)?;
        for row in rows {
            self.check_cancelled()?;
            entries.push(PostingEntry::new(
                row.doc_id,
                Payload {
                    score: row.score,
                    ..Payload::default()
                },
            ));
        }
        self.release_bytes(capacity * size_of::<ScoredEntry>());
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    pub(super) fn release_bytes(&mut self, bytes: usize) {
        self.used = self
            .used
            .checked_sub(bytes)
            .expect("balanced phrase memory reservation");
    }

    pub(super) fn reserve_items<T>(&mut self, count: usize) -> PhraseResult<()> {
        let bytes = count
            .checked_mul(size_of::<T>())
            .ok_or(PhraseError::MemoryLimit {
                required: usize::MAX,
                limit: self.limit,
            })?;
        self.reserve_bytes(bytes)
    }

    pub(super) fn grow<T>(&mut self, values: &mut Vec<T>) -> PhraseResult<()> {
        self.check_cancelled()?;
        if values.len() < values.capacity() {
            return Ok(());
        }
        // Reserve the old and new allocation together while the vector can move.
        let capacity = values
            .capacity()
            .max(1)
            .checked_mul(2)
            .ok_or(PhraseError::MemoryLimit {
                required: usize::MAX,
                limit: self.limit,
            })?;
        self.reserve_items::<T>(capacity)?;
        let old = values.capacity();
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(PhraseError::Allocation)?;
        self.release_bytes(old * size_of::<T>());
        if values.capacity() > capacity {
            self.reserve_items::<T>(values.capacity() - capacity)?;
        }
        Ok(())
    }
}
