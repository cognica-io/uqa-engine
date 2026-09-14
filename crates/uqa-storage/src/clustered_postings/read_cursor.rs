//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A forward cursor that may borrow a retained index instead of materializing its postings.

use super::{DocId, PostingCursor, PostingScore, StorageBackendResult};
use crate::read_control::StorageReadControl;
use uqa_core::memory::MemoryReservation;

/// A unique cursor owner whose box payload is reserved before allocation.
pub struct BudgetedPostingReadCursor<'a> {
    cursor: Box<dyn PostingReadCursor + 'a>,
    // Drop the cursor and its internal buffers before returning this payload reservation.
    _memory: MemoryReservation,
}
impl<'a> BudgetedPostingReadCursor<'a> {
    pub fn new<T: PostingReadCursor + 'a>(
        cursor: T,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<T>())?;
        Ok(Self {
            cursor: Box::new(cursor),
            _memory: memory,
        })
    }
}
impl PostingReadCursor for BudgetedPostingReadCursor<'_> {
    fn doc_freq(&self) -> u64 {
        self.cursor.doc_freq()
    }
    fn current(&self) -> Option<PostingScore> {
        self.cursor.current()
    }
    fn advance(&mut self) -> StorageBackendResult<Option<PostingScore>> {
        self.cursor.advance()
    }
    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<Option<PostingScore>> {
        self.cursor.advance_to(target)
    }
}

/// Read-only candidate traversal bounded by the lifetime of the retained index read.
pub trait PostingReadCursor: Send {
    fn doc_freq(&self) -> u64;
    fn current(&self) -> Option<PostingScore>;
    fn advance(&mut self) -> StorageBackendResult<Option<PostingScore>>;
    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<Option<PostingScore>>;
}

/// Adapt an owned provider cursor without changing its incremental read strategy.
pub struct OwnedPostingReadCursor(pub Box<dyn PostingCursor>);

impl PostingReadCursor for OwnedPostingReadCursor {
    fn doc_freq(&self) -> u64 {
        self.0.doc_freq()
    }
    fn current(&self) -> Option<PostingScore> {
        self.0.current()
    }
    fn advance(&mut self) -> StorageBackendResult<Option<PostingScore>> {
        self.0.advance()
    }
    fn advance_to(&mut self, target: DocId) -> StorageBackendResult<Option<PostingScore>> {
        self.0.advance_to(target)
    }
}
