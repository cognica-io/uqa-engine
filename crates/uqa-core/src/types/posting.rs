//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Posting and generalized-posting entry payload types.

use super::{BTreeMap, DocId, FieldName, Value};

/// Posting list entry payload: token positions, relevance score, and any
/// extra field values the operator pipeline carries forward.
///
/// `positions` is sorted ascending with no duplicates. `fields` uses
/// `BTreeMap` (not `HashMap`) so equality and iteration are deterministic
/// across storage, merge, and regression tests.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Payload {
    pub positions: Vec<u32>,
    pub score: f64,
    pub fields: BTreeMap<FieldName, Value>,
}

impl Payload {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_score(score: f64) -> Self {
        Self {
            score,
            ..Self::default()
        }
    }
}

/// A single `(doc_id, payload)` entry in a posting list.
#[derive(Debug, Clone, PartialEq)]
pub struct PostingEntry {
    pub doc_id: DocId,
    pub payload: Payload,
}

impl PostingEntry {
    pub fn new(doc_id: DocId, payload: Payload) -> Self {
        Self { doc_id, payload }
    }
}

/// Join result entry with multi-document tuples (Definition 4.1.2, Paper 1).
///
/// `doc_ids` is ordered the same way as the joined relations contributed to
/// the result; equality and ordering are tuple-wise.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GeneralizedPostingEntry {
    pub doc_ids: Vec<DocId>,
    pub payload: GeneralizedPayload,
}

/// Payload for `GeneralizedPostingEntry`. Carries no floating-point
/// score, so `Eq`/`Ord` derive cleanly and joined entries can key directly
/// off `(doc_ids, payload)` without a separate ordering helper.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct GeneralizedPayload {
    pub fields: BTreeMap<FieldName, Value>,
}
