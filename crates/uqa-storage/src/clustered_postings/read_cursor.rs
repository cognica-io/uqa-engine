//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A forward cursor that may borrow a retained index instead of materializing its postings.

use super::{DocId, PostingCursor, PostingScore, StorageBackendResult};

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
