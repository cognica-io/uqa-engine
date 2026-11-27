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

use std::collections::{BTreeMap, BTreeSet};

use uqa_analysis::analyzer::standard_term_analyzer;
use uqa_core::{
    GeneralizedPayload, GeneralizedPostingEntry, GeneralizedPostingList, PostingEntry, Value,
};
use uqa_graph::{
    CrossParadigmError, CrossParadigmResult, Direction, GraphStore, GraphStoreError,
    GraphStoreResult,
};

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

    pub fn execute(&self) -> CrossParadigmResult<GeneralizedPostingList> {
        validate_threshold(self.threshold, 0.0, 1.0, "text similarity")?;
        let mut left_texts = self
            .left
            .iter()
            .filter_map(|entry| text_field(entry, self.left_field))
            .peekable();
        if left_texts.peek().is_none() {
            return Ok(GeneralizedPostingList::from_unsorted(Vec::new()));
        }
        let analyze = standard_term_analyzer(self.language)?;
        let mut right_tokens = None;
        let mut out = Vec::new();
        for (left, text) in left_texts {
            let left_tokens: BTreeSet<String> = analyze(text)?.into_iter().collect();
            if left_tokens.is_empty() {
                continue;
            }
            let rights = match &right_tokens {
                Some(rights) => rights,
                None => {
                    let mut prepared = Vec::new();
                    for (entry, text) in self
                        .right
                        .iter()
                        .filter_map(|entry| text_field(entry, self.right_field))
                    {
                        let tokens: BTreeSet<String> = analyze(text)?.into_iter().collect();
                        if !tokens.is_empty() {
                            prepared.push((entry, tokens));
                        }
                    }
                    right_tokens.insert(prepared)
                }
            };
            for (right, tokens) in rights {
                let inter = left_tokens.intersection(tokens).count();
                let union = (left_tokens.len() - inter) + tokens.len();
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
        Ok(GeneralizedPostingList::from_unsorted(out))
    }
}

fn text_field<'a>(entry: &'a PostingEntry, field: &str) -> Option<(&'a PostingEntry, &'a str)> {
    match entry.payload.fields.get(field)? {
        Value::Str(text) => Some((entry, text.as_str())),
        _ => None,
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

    pub fn execute(&self) -> CrossParadigmResult<GeneralizedPostingList> {
        validate_threshold(self.threshold, -1.0, 1.0, "vector similarity")?;
        let mut out = Vec::new();
        let mut rights = None;
        for left in self.left {
            let Some(value) = left.payload.fields.get(self.left_field) else {
                continue;
            };
            let left_vec = PreparedVector::read(value, self.left_field)?;
            let rights = rights.get_or_insert_with(|| {
                self.right
                    .iter()
                    .filter_map(|entry| RightVector::new(entry, self.right_field))
                    .collect::<Vec<_>>()
            });
            for right in rights {
                let cosine = right.cosine(&left_vec, self.right_field)?;
                let right = right.entry;
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
        Ok(GeneralizedPostingList::from_unsorted(out))
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

    pub fn execute(&self) -> CrossParadigmResult<GeneralizedPostingList> {
        validate_threshold(self.threshold, -1.0, 1.0, "hybrid")?;
        let mut right_index: BTreeMap<&Value, Vec<RightVector<'_>>> = BTreeMap::new();
        for entry in self.right {
            if let Some(key) = entry.payload.fields.get(self.structured_field) {
                if let Some(vector) = RightVector::new(entry, self.vector_field) {
                    right_index.entry(key).or_default().push(vector);
                }
            }
        }
        let mut out = Vec::new();
        for left in self.left {
            let Some(left_key) = left.payload.fields.get(self.structured_field) else {
                continue;
            };
            let Some(value) = left.payload.fields.get(self.vector_field) else {
                continue;
            };
            let left_vec = PreparedVector::read(value, self.vector_field)?;
            let Some(rights) = right_index.get_mut(left_key) else {
                continue;
            };
            for right in rights {
                let cosine = right.cosine(&left_vec, self.vector_field)?;
                let right = right.entry;
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
        Ok(GeneralizedPostingList::from_unsorted(out))
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

    pub fn execute(&self) -> GraphStoreResult<GeneralizedPostingList> {
        let right_by_id: BTreeMap<u64, &PostingEntry> =
            self.right.iter().map(|e| (e.doc_id, e)).collect();
        let mut out = Vec::new();
        for left in self.left {
            if !left.payload.score.is_finite() {
                return Err(GraphStoreError::InvalidMutation(format!(
                    "graph join left score for document {} must be finite",
                    left.doc_id
                )));
            }
            for neighbor_id in
                self.store
                    .neighbors(left.doc_id, self.label, Direction::Out, self.graph)?
            {
                let Some(right) = right_by_id.get(&neighbor_id) else {
                    continue;
                };
                if !right.payload.score.is_finite() {
                    return Err(GraphStoreError::InvalidMutation(format!(
                        "graph join right score for document {} must be finite",
                        right.doc_id
                    )));
                }
                let merged_score = left.payload.score + right.payload.score;
                if !merged_score.is_finite() {
                    return Err(GraphStoreError::InvalidMutation(format!(
                        "graph join score overflow for documents {} and {}",
                        left.doc_id, right.doc_id
                    )));
                }
                out.push(make_entry(
                    left.doc_id,
                    right.doc_id,
                    &left.payload.fields,
                    &right.payload.fields,
                    merged_score,
                ));
            }
        }
        Ok(GeneralizedPostingList::from_unsorted(out))
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

    pub fn execute(&self) -> CrossParadigmResult<GeneralizedPostingList> {
        for entry in self.left {
            validate_payload_score(entry, "left")?;
        }
        for entry in self.right {
            validate_payload_score(entry, "right")?;
        }
        let mut right_index: BTreeMap<&Value, Vec<&PostingEntry>> = BTreeMap::new();
        for entry in self.right {
            if let Some(key) = entry.payload.fields.get(self.doc_field) {
                right_index.entry(key).or_default().push(entry);
            }
        }
        let mut out = Vec::new();
        for left in self.left {
            let vertex = self.store.get_vertex(left.doc_id)?.ok_or_else(|| {
                CrossParadigmError::GraphStore(GraphStoreError::CorruptGraph(format!(
                    "cross-paradigm join input references missing graph vertex {}",
                    left.doc_id
                )))
            })?;
            let Some(vertex_key) = vertex.properties.get(self.vertex_field) else {
                continue;
            };
            let Some(rights) = right_index.get(vertex_key) else {
                continue;
            };
            for right in rights {
                let mut merged: BTreeMap<String, Value> = BTreeMap::new();
                for (k, val) in &vertex.properties {
                    merged.insert(k.clone(), val.clone());
                }
                for (k, val) in &left.payload.fields {
                    merged.insert(k.clone(), val.clone());
                }
                for (k, val) in &right.payload.fields {
                    merged.insert(k.clone(), val.clone());
                }
                let merged_score = left.payload.score + right.payload.score;
                if !merged_score.is_finite() {
                    return Err(CrossParadigmError::ArithmeticOverflow(format!(
                        "joined score for documents {} and {} is not finite",
                        left.doc_id, right.doc_id
                    )));
                }
                merged.insert(SCORE_FIELD.into(), Value::Float(merged_score));
                out.push(GeneralizedPostingEntry {
                    doc_ids: vec![left.doc_id, right.doc_id],
                    payload: GeneralizedPayload { fields: merged },
                });
            }
        }
        Ok(GeneralizedPostingList::from_unsorted(out))
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

fn validate_threshold(
    threshold: f64,
    minimum: f64,
    maximum: f64,
    context: &str,
) -> CrossParadigmResult<()> {
    if !threshold.is_finite() || !(minimum..=maximum).contains(&threshold) {
        return Err(CrossParadigmError::InvalidInput(format!(
            "{context} threshold must be finite and in [{minimum}, {maximum}], got {threshold}"
        )));
    }
    Ok(())
}

fn validate_payload_score(entry: &PostingEntry, side: &str) -> CrossParadigmResult<()> {
    if !entry.payload.score.is_finite() {
        return Err(CrossParadigmError::InvalidInput(format!(
            "cross-paradigm join {side} score for document {} must be finite",
            entry.doc_id
        )));
    }
    Ok(())
}

struct RightVector<'a> {
    entry: &'a PostingEntry,
    value: &'a Value,
    prepared: Option<PreparedVector>,
}

impl<'a> RightVector<'a> {
    fn new(entry: &'a PostingEntry, name: &str) -> Option<Self> {
        Some(Self {
            entry,
            value: entry.payload.fields.get(name)?,
            prepared: None,
        })
    }

    fn cosine(&mut self, left: &PreparedVector, name: &str) -> CrossParadigmResult<f64> {
        // Validate on first use, in the original left-major pair order.
        let right = match &self.prepared {
            Some(vector) => vector,
            None => self
                .prepared
                .insert(PreparedVector::read(self.value, name)?),
        };
        left.cosine(right)
    }
}

struct PreparedVector {
    components: Vec<f64>,
    zero_norm: bool,
}

impl PreparedVector {
    fn read(value: &Value, name: &str) -> CrossParadigmResult<Self> {
        let Value::List(items) = value else {
            return Err(CrossParadigmError::InvalidInput(format!(
                "vector field {name:?} must be a list"
            )));
        };
        if items.is_empty() {
            return Err(CrossParadigmError::InvalidInput(format!(
                "vector field {name:?} must not be empty"
            )));
        }
        let mut out = Vec::new();
        out.try_reserve_exact(items.len()).map_err(|error| {
            CrossParadigmError::InvalidInput(format!(
                "cannot allocate vector field {name:?} with {} components: {error}",
                items.len()
            ))
        })?;
        for v in items {
            match v {
                Value::Float(f) if f.is_finite() => out.push(*f),
                Value::Int(n) if n.unsigned_abs() <= (1_u64 << 53) => out.push(*n as f64),
                Value::Int(n) => {
                    return Err(CrossParadigmError::InvalidInput(format!(
                        "integer vector component {n} in field {name:?} cannot be represented exactly as f64"
                    )));
                }
                _ => {
                    return Err(CrossParadigmError::InvalidInput(format!(
                        "vector field {name:?} contains a non-numeric or non-finite component"
                    )));
                }
            }
        }
        let scale = out
            .iter()
            .fold(0.0_f64, |scale, value| scale.max(value.abs()));
        if scale != 0.0 {
            let norm = out
                .iter()
                .map(|value| (value / scale).powi(2))
                .sum::<f64>()
                .sqrt();
            for component in &mut out {
                *component = *component / scale / norm;
            }
        }
        Ok(Self {
            components: out,
            zero_norm: scale == 0.0,
        })
    }

    fn cosine(&self, other: &Self) -> CrossParadigmResult<f64> {
        if self.components.len() != other.components.len() {
            return Err(CrossParadigmError::InvalidInput(format!(
                "cosine vectors must have the same non-zero dimension ({} != {})",
                self.components.len(),
                other.components.len()
            )));
        }
        if self.zero_norm || other.zero_norm {
            return Err(CrossParadigmError::InvalidInput(
                "cosine vectors must have non-zero norms".into(),
            ));
        }
        let cosine: f64 = self
            .components
            .iter()
            .zip(&other.components)
            .map(|(left, right)| left * right)
            .sum();
        if !cosine.is_finite() {
            return Err(CrossParadigmError::ArithmeticOverflow(
                "cosine similarity is not finite".into(),
            ));
        }
        Ok(cosine.clamp(-1.0, 1.0))
    }
}
