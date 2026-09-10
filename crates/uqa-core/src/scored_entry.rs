//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document identity and score produced by a retrieval operator.

use crate::{DocId, PostingEntry};

#[derive(Debug, Clone)]
pub struct ScoredEntry {
    pub doc_id: DocId,
    pub score: f64,
}

impl ScoredEntry {
    pub fn from_entry(e: &PostingEntry) -> Self {
        Self {
            doc_id: e.doc_id,
            score: e.payload.score,
        }
    }
}
