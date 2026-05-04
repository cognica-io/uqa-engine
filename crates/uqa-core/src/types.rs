//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core value types for UQA: doc ids, payloads, posting entries, and the
//! dynamic [`Value`] used inside payload fields.

use std::collections::BTreeMap;

/// Document identifier.
///
/// `u64` addresses up to ~1.8e19 documents while keeping the on-disk
/// representation compact at 8 bytes per posting entry head.
pub type DocId = u64;

/// Field name within a document.
pub type FieldName = String;

/// Dynamic value type for document fields and posting payload extras.
///
/// Covers the JSON-like values the engine round-trips through a posting
/// list. Date and datetime variants land with the SQL type system.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

/// Posting list entry payload: token positions, relevance score, and any
/// extra field values the operator pipeline carries forward.
///
/// `positions` is sorted ascending with no duplicates. `fields` uses
/// `BTreeMap` (not `HashMap`) so equality and iteration are deterministic
/// — this matters for the Boolean-algebra property tests.
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

/// Index-level statistics consumed by the cost model and BM25 scorer.
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub total_docs: u64,
    pub avg_doc_length: f64,
    pub dimensions: u32,
    doc_freqs: BTreeMap<(FieldName, String), u64>,
}

impl IndexStats {
    pub fn doc_freq(&self, field: &str, term: &str) -> u64 {
        self.doc_freqs
            .get(&(field.to_string(), term.to_string()))
            .copied()
            .unwrap_or(0)
    }

    pub fn set_doc_freq(&mut self, field: impl Into<FieldName>, term: impl Into<String>, df: u64) {
        self.doc_freqs.insert((field.into(), term.into()), df);
    }
}

// `Value` carries `f64`, which is not `Eq` or `Hash`. We provide a total
// order anyway: `PartialOrd::partial_cmp` on floats falls back to `Equal`
// for NaN. Joins that need to compare on floating values must route them
// through scoring; the order here is only for keying joined-entry tuples.
impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Value::Str(a), Value::Str(b)) => a.cmp(b),
            (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
            (Value::List(a), Value::List(b)) => a.cmp(b),
            (Value::Map(a), Value::Map(b)) => a.cmp(b),
            _ => discriminant(self).cmp(&discriminant(other)),
        }
    }
}

fn discriminant(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Int(_) => 2,
        Value::Float(_) => 3,
        Value::Str(_) => 4,
        Value::Bytes(_) => 5,
        Value::List(_) => 6,
        Value::Map(_) => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_default_is_zero_score_and_empty() {
        let p = Payload::default();
        assert_eq!(p.score, 0.0);
        assert!(p.positions.is_empty());
        assert!(p.fields.is_empty());
    }

    #[test]
    fn posting_entry_construction_round_trips() {
        let e = PostingEntry::new(42, Payload::with_score(1.5));
        assert_eq!(e.doc_id, 42);
        let diff: f64 = e.payload.score - 1.5;
        assert!(diff.abs() < f64::EPSILON);
    }

    #[test]
    fn index_stats_doc_freq_default_zero() {
        let s = IndexStats::default();
        assert_eq!(s.doc_freq("title", "rust"), 0);
    }

    #[test]
    fn index_stats_records_doc_freq() {
        let mut s = IndexStats::default();
        s.set_doc_freq("title", "rust", 12);
        assert_eq!(s.doc_freq("title", "rust"), 12);
        assert_eq!(s.doc_freq("title", "java"), 0);
    }

    #[test]
    fn generalized_entry_orders_lexicographically() {
        let a = GeneralizedPostingEntry {
            doc_ids: vec![1, 2],
            payload: GeneralizedPayload::default(),
        };
        let b = GeneralizedPostingEntry {
            doc_ids: vec![1, 3],
            payload: GeneralizedPayload::default(),
        };
        assert!(a < b);
    }

    #[test]
    fn value_ordering_within_variant() {
        assert!(Value::Int(1) < Value::Int(2));
        assert!(Value::Str("a".into()) < Value::Str("b".into()));
    }

    #[test]
    fn value_ordering_across_variants_is_stable() {
        assert!(Value::Null < Value::Bool(false));
        assert!(Value::Bool(true) < Value::Int(0));
    }
}
