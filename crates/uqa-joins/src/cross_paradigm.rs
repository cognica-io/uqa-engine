//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-paradigm join operators.
//!
//! Each operator consumes two `&[PostingEntry]` inputs (`left`, `right`)
//! and emits a [`GeneralizedPostingList`] of joined `(left_id,
//! right_id)` tuples with merged payload fields and a fused score
//! recorded into `Payload.fields["_score"]`. The Rust
//! `GeneralizedPayload` type carries no float `score`, so we lift the
//! similarity score into a payload field; downstream consumers can
//! pull it back out for ranking.

use std::collections::BTreeMap;

use uqa_analysis::analyzer::standard_analyzer;
use uqa_core::{
    GeneralizedPayload, GeneralizedPostingEntry, GeneralizedPostingList, PostingEntry, Value,
};
use uqa_graph::{Direction, GraphStore};

const SCORE_FIELD: &str = "_score";

/// Jaccard similarity over tokenized text fields.
pub struct TextSimilarityJoin<'a> {
    pub left: &'a [PostingEntry],
    pub right: &'a [PostingEntry],
    pub left_field: &'a str,
    pub right_field: &'a str,
    pub threshold: f64,
    pub language: &'a str,
}

impl<'a> TextSimilarityJoin<'a> {
    pub fn new(
        left: &'a [PostingEntry],
        right: &'a [PostingEntry],
        left_field: &'a str,
        right_field: &'a str,
    ) -> Self {
        Self {
            left,
            right,
            left_field,
            right_field,
            threshold: 0.5,
            language: "english",
        }
    }

    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = t;
        self
    }

    pub fn language(mut self, lang: &'a str) -> Self {
        self.language = lang;
        self
    }

    pub fn execute(&self) -> GeneralizedPostingList {
        let analyzer = standard_analyzer(self.language);
        let mut out: Vec<GeneralizedPostingEntry> = Vec::new();
        for left in self.left {
            let Some(Value::Str(left_text)) = left.payload.fields.get(self.left_field) else {
                continue;
            };
            let left_tokens: std::collections::BTreeSet<String> =
                analyzer.analyze(left_text).into_iter().collect();
            if left_tokens.is_empty() {
                continue;
            }
            for right in self.right {
                let Some(Value::Str(right_text)) = right.payload.fields.get(self.right_field)
                else {
                    continue;
                };
                let right_tokens: std::collections::BTreeSet<String> =
                    analyzer.analyze(right_text).into_iter().collect();
                if right_tokens.is_empty() {
                    continue;
                }
                let inter = left_tokens.intersection(&right_tokens).count();
                let union = left_tokens.union(&right_tokens).count();
                if union == 0 {
                    continue;
                }
                let jaccard = inter as f64 / union as f64;
                if jaccard < self.threshold {
                    continue;
                }
                out.push(make_entry(
                    left.doc_id,
                    right.doc_id,
                    &left.payload.fields,
                    &right.payload.fields,
                    jaccard,
                ));
            }
        }
        GeneralizedPostingList::from_unsorted(out)
    }
}

/// Cosine similarity join over numeric vector fields.
pub struct VectorSimilarityJoin<'a> {
    pub left: &'a [PostingEntry],
    pub right: &'a [PostingEntry],
    pub left_field: &'a str,
    pub right_field: &'a str,
    pub threshold: f64,
}

impl<'a> VectorSimilarityJoin<'a> {
    pub fn new(
        left: &'a [PostingEntry],
        right: &'a [PostingEntry],
        left_field: &'a str,
        right_field: &'a str,
    ) -> Self {
        Self {
            left,
            right,
            left_field,
            right_field,
            threshold: 0.5,
        }
    }

    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = t;
        self
    }

    pub fn execute(&self) -> GeneralizedPostingList {
        let mut out = Vec::new();
        for left in self.left {
            let Some(left_vec) = read_vector(&left.payload.fields, self.left_field) else {
                continue;
            };
            let left_norm = l2_norm(&left_vec);
            if left_norm == 0.0 {
                continue;
            }
            for right in self.right {
                let Some(right_vec) = read_vector(&right.payload.fields, self.right_field) else {
                    continue;
                };
                let right_norm = l2_norm(&right_vec);
                if right_norm == 0.0 {
                    continue;
                }
                let cosine = dot(&left_vec, &right_vec) / (left_norm * right_norm);
                if cosine < self.threshold {
                    continue;
                }
                out.push(make_entry(
                    left.doc_id,
                    right.doc_id,
                    &left.payload.fields,
                    &right.payload.fields,
                    cosine,
                ));
            }
        }
        GeneralizedPostingList::from_unsorted(out)
    }
}

/// Structured equijoin combined with cosine similarity. Right side is
/// hashed by `structured_field` to avoid the full nested loop; cosine
/// similarity then gates each candidate pair.
pub struct HybridJoin<'a> {
    pub left: &'a [PostingEntry],
    pub right: &'a [PostingEntry],
    pub structured_field: &'a str,
    pub vector_field: &'a str,
    pub threshold: f64,
}

impl<'a> HybridJoin<'a> {
    pub fn new(
        left: &'a [PostingEntry],
        right: &'a [PostingEntry],
        structured_field: &'a str,
        vector_field: &'a str,
    ) -> Self {
        Self {
            left,
            right,
            structured_field,
            vector_field,
            threshold: 0.5,
        }
    }

    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = t;
        self
    }

    pub fn execute(&self) -> GeneralizedPostingList {
        let mut right_index: BTreeMap<Value, Vec<&PostingEntry>> = BTreeMap::new();
        for entry in self.right {
            if let Some(key) = entry.payload.fields.get(self.structured_field) {
                right_index.entry(key.clone()).or_default().push(entry);
            }
        }
        let mut out = Vec::new();
        for left in self.left {
            let Some(left_key) = left.payload.fields.get(self.structured_field) else {
                continue;
            };
            let Some(left_vec) = read_vector(&left.payload.fields, self.vector_field) else {
                continue;
            };
            let left_norm = l2_norm(&left_vec);
            if left_norm == 0.0 {
                continue;
            }
            let Some(rights) = right_index.get(left_key) else {
                continue;
            };
            for right in rights {
                let Some(right_vec) = read_vector(&right.payload.fields, self.vector_field) else {
                    continue;
                };
                let right_norm = l2_norm(&right_vec);
                if right_norm == 0.0 {
                    continue;
                }
                let cosine = dot(&left_vec, &right_vec) / (left_norm * right_norm);
                if cosine < self.threshold {
                    continue;
                }
                out.push(make_entry(
                    left.doc_id,
                    right.doc_id,
                    &left.payload.fields,
                    &right.payload.fields,
                    cosine,
                ));
            }
        }
        GeneralizedPostingList::from_unsorted(out)
    }
}

/// Join two posting lists by graph edge connectivity. For each
/// `(left, right)` pair, emit the joined entry when an edge from
/// `left.doc_id` to `right.doc_id` exists in `graph` (optionally
/// filtered by `label`). Score is the sum of the two side scores so
/// downstream rankers can use the natural log-odds aggregation.
pub struct GraphJoin<'a, G: GraphStore> {
    pub left: &'a [PostingEntry],
    pub right: &'a [PostingEntry],
    pub store: &'a G,
    pub graph: &'a str,
    pub label: Option<&'a str>,
}

impl<'a, G: GraphStore> GraphJoin<'a, G> {
    pub fn new(
        left: &'a [PostingEntry],
        right: &'a [PostingEntry],
        store: &'a G,
        graph: &'a str,
    ) -> Self {
        Self {
            left,
            right,
            store,
            graph,
            label: None,
        }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    pub fn execute(&self) -> GeneralizedPostingList {
        let right_by_id: BTreeMap<u64, &PostingEntry> =
            self.right.iter().map(|e| (e.doc_id, e)).collect();
        let mut out = Vec::new();
        for left in self.left {
            for neighbor_id in
                self.store
                    .neighbors(left.doc_id, self.label, Direction::Out, self.graph)
            {
                let Some(right) = right_by_id.get(&neighbor_id) else {
                    continue;
                };
                let merged_score = left.payload.score + right.payload.score;
                out.push(make_entry(
                    left.doc_id,
                    right.doc_id,
                    &left.payload.fields,
                    &right.payload.fields,
                    merged_score,
                ));
            }
        }
        GeneralizedPostingList::from_unsorted(out)
    }
}

/// Bridge graph vertices to document payloads: read a property
/// (`vertex_field`) off each left vertex and look up matching documents
/// on the right side keyed by `doc_field`. The vertex's properties
/// are folded into the output's payload before the document fields.
pub struct CrossParadigmJoin<'a, G: GraphStore> {
    pub left: &'a [PostingEntry],
    pub right: &'a [PostingEntry],
    pub store: &'a G,
    pub vertex_field: &'a str,
    pub doc_field: &'a str,
}

impl<'a, G: GraphStore> CrossParadigmJoin<'a, G> {
    pub fn new(
        left: &'a [PostingEntry],
        right: &'a [PostingEntry],
        store: &'a G,
        vertex_field: &'a str,
        doc_field: &'a str,
    ) -> Self {
        Self {
            left,
            right,
            store,
            vertex_field,
            doc_field,
        }
    }

    pub fn execute(&self) -> GeneralizedPostingList {
        let mut right_index: BTreeMap<Value, Vec<&PostingEntry>> = BTreeMap::new();
        for entry in self.right {
            if let Some(key) = entry.payload.fields.get(self.doc_field) {
                right_index.entry(key.clone()).or_default().push(entry);
            }
        }
        let mut out = Vec::new();
        for left in self.left {
            let vertex = self.store.get_vertex(left.doc_id);
            let vertex_key: Option<Value> = match vertex {
                Some(v) => v.properties.get(self.vertex_field).cloned(),
                None => left.payload.fields.get(self.vertex_field).cloned(),
            };
            let Some(vertex_key) = vertex_key else {
                continue;
            };
            let Some(rights) = right_index.get(&vertex_key) else {
                continue;
            };
            for right in rights {
                let mut merged: BTreeMap<String, Value> = BTreeMap::new();
                if let Some(v) = vertex {
                    for (k, val) in &v.properties {
                        merged.insert(k.clone(), val.clone());
                    }
                }
                for (k, val) in &left.payload.fields {
                    merged.insert(k.clone(), val.clone());
                }
                for (k, val) in &right.payload.fields {
                    merged.insert(k.clone(), val.clone());
                }
                let merged_score = left.payload.score + right.payload.score;
                merged.insert(SCORE_FIELD.into(), Value::Float(merged_score));
                out.push(GeneralizedPostingEntry {
                    doc_ids: vec![left.doc_id, right.doc_id],
                    payload: GeneralizedPayload { fields: merged },
                });
            }
        }
        GeneralizedPostingList::from_unsorted(out)
    }
}

fn make_entry(
    left_id: u64,
    right_id: u64,
    left_fields: &BTreeMap<String, Value>,
    right_fields: &BTreeMap<String, Value>,
    score: f64,
) -> GeneralizedPostingEntry {
    let mut merged: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in left_fields {
        merged.insert(k.clone(), v.clone());
    }
    for (k, v) in right_fields {
        merged.insert(k.clone(), v.clone());
    }
    merged.insert(SCORE_FIELD.into(), Value::Float(score));
    GeneralizedPostingEntry {
        doc_ids: vec![left_id, right_id],
        payload: GeneralizedPayload { fields: merged },
    }
}

fn read_vector(fields: &BTreeMap<String, Value>, name: &str) -> Option<Vec<f64>> {
    let Value::List(items) = fields.get(name)? else {
        return None;
    };
    let mut out = Vec::with_capacity(items.len());
    for v in items {
        match v {
            Value::Float(f) => out.push(*f),
            Value::Int(n) => out.push(*n as f64),
            _ => return None,
        }
    }
    Some(out)
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let mut s = 0.0;
    for i in 0..n {
        s += a[i] * b[i];
    }
    s
}

fn l2_norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}
