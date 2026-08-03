//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primitive operators: [`TermOperator`] (Definition 3.1.1),
//! [`FilterOperator`] (Definition 3.1.4), [`FacetOperator`]
//! (Definition 3.1.5), [`ScoreOperator`] (Definition 3.1.6).

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{
    DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList, Predicate, Value,
};
use uqa_scoring::Scorer;
use uqa_storage::StorageBackendError;

use crate::base::{
    missing_backend, require_finite_score, ExecutionContext, Operator, OperatorResult,
};

/// `T(t) = PL({d in D | t in term(d, f)})`.
///
/// Resolves the search-time analyzer for `field`, runs it over `term`,
/// looks up each resulting token's posting list, and unions them.
pub struct TermOperator {
    pub term: String,
    pub field: String,
}

impl TermOperator {
    pub fn new(term: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            term: term.into(),
            field: field.into(),
        }
    }
}

impl Operator for TermOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let Some(idx) = ctx.inverted_index.as_ref() else {
            return Err(missing_backend("inverted-index", "term search"));
        };
        // Search-time analyzer: synonym filters and similar transforms expand
        // `term` into tokens that are unioned across the field's posting lists.
        let analyzer = idx.get_search_analyzer(&self.field);
        let tokens = analyzer.analyze(&self.term)?;
        if tokens.is_empty() {
            return Ok(PostingList::new());
        }
        let mut acc = idx.get_posting_list(&self.field, &tokens[0])?;
        for t in &tokens[1..] {
            acc = acc.merge_union(&idx.get_posting_list(&self.field, t)?);
        }
        Ok(acc)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        stats.doc_freq(&self.field, &self.term) as f64
    }
}

/// `SpatialWithin_{f, center, distance}`: return all documents whose
/// `field` value lies within `distance` (great-circle metres) of
/// `(center_x, center_y)`. Brute-force
/// scans the document store using
/// [`uqa_storage::haversine_distance`]; spatial indexes plug in via
/// the engine layer.
pub struct SpatialWithinOperator {
    pub field: String,
    pub center_x: f64,
    pub center_y: f64,
    pub distance: f64,
}

impl SpatialWithinOperator {
    pub fn new(field: impl Into<String>, center_x: f64, center_y: f64, distance: f64) -> Self {
        Self {
            field: field.into(),
            center_x,
            center_y,
            distance,
        }
    }
}

impl Operator for SpatialWithinOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        if !self.center_x.is_finite()
            || !self.center_y.is_finite()
            || !self.distance.is_finite()
            || self.distance < 0.0
        {
            return Err(StorageBackendError::Other(format!(
                "spatial filter requires finite coordinates and a non-negative finite distance, got ({}, {}) distance {}",
                self.center_x, self.center_y, self.distance
            )));
        }
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return Err(missing_backend("document-store", "spatial filter"));
        };
        let mut entries: Vec<PostingEntry> = Vec::new();
        let mut ids = doc_store.doc_ids()?;
        ids.sort_unstable();
        for doc_id in ids {
            if doc_store.get(doc_id)?.is_none() {
                return Err(StorageBackendError::Other(format!(
                    "spatial filter candidate {doc_id} is missing from the document store"
                )));
            }
            let Some(pt) = doc_store.get_field(doc_id, &self.field)? else {
                continue;
            };
            let coords = match &pt {
                Value::List(items) if items.len() == 2 => items,
                _ => {
                    return Err(StorageBackendError::Other(format!(
                    "spatial field {:?} for document {doc_id} must be a two-component numeric list",
                    self.field
                )))
                }
            };
            let (Some(x), Some(y)) = (value_to_f64(&coords[0]), value_to_f64(&coords[1])) else {
                return Err(StorageBackendError::Other(format!(
                    "spatial field {:?} for document {doc_id} contains a non-numeric coordinate",
                    self.field
                )));
            };
            if !x.is_finite() || !y.is_finite() {
                return Err(StorageBackendError::Other(format!(
                    "spatial field {:?} for document {doc_id} contains a non-finite coordinate",
                    self.field
                )));
            }
            let dist = uqa_storage::haversine_distance(self.center_x, self.center_y, x, y);
            if dist <= self.distance {
                let score = if self.distance > 0.0 {
                    1.0 - (dist / self.distance)
                } else {
                    1.0
                };
                entries.push(PostingEntry::new(
                    doc_id,
                    Payload {
                        score,
                        ..Default::default()
                    },
                ));
            }
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        ((stats.total_docs + 1) as f64).log2()
    }
}

fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// `Filter_{f, predicate}`: filter a source posting list (or the universe
/// of documents) by applying a predicate to a field.
pub struct FilterOperator {
    pub field: String,
    pub predicate: Predicate,
    pub source: Option<Arc<dyn Operator>>,
}

impl FilterOperator {
    pub fn new(
        field: impl Into<String>,
        predicate: Predicate,
        source: Option<Arc<dyn Operator>>,
    ) -> Self {
        Self {
            field: field.into(),
            predicate,
            source,
        }
    }
}

impl Operator for FilterOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return Err(missing_backend("document-store", "field filter"));
        };
        let null_aware = self.predicate.is_null_aware();

        let candidates: Vec<PostingEntry> = if let Some(src) = &self.source {
            src.execute(ctx)?.into_iter().collect()
        } else {
            doc_store
                .doc_ids()?
                .into_iter()
                .map(|id| PostingEntry::new(id, Payload::default()))
                .collect()
        };

        let mut out = Vec::with_capacity(candidates.len());
        for entry in candidates {
            if doc_store.get(entry.doc_id)?.is_none() {
                return Err(StorageBackendError::Other(format!(
                    "field filter candidate {} is missing from the document store",
                    entry.doc_id
                )));
            }
            let value = doc_store.get_field(entry.doc_id, &self.field)?;
            let matched = if null_aware {
                self.predicate.evaluate(value.as_ref())
            } else {
                value.is_some() && self.predicate.evaluate(value.as_ref())
            };
            if matched {
                out.push(entry);
            }
        }
        Ok(PostingList::from_sorted_unchecked(out))
    }
}

/// `Facet_f`: count distinct values of a field over a source posting list
/// (or the entire document store). The result is a posting list whose
/// `payload.fields` carry `_facet_field`, `_facet_value`, `_facet_count`,
/// matching the serialized UQA encoding.
pub struct FacetOperator {
    pub field: String,
    pub source: Option<Arc<dyn Operator>>,
}

impl FacetOperator {
    pub fn new(field: impl Into<String>, source: Option<Arc<dyn Operator>>) -> Self {
        Self {
            field: field.into(),
            source,
        }
    }
}

impl Operator for FacetOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return Err(missing_backend("document-store", "facet aggregation"));
        };

        let candidate_ids: Vec<DocId> = if let Some(src) = &self.source {
            src.execute(ctx)?.doc_ids().collect()
        } else {
            doc_store.doc_ids()?
        };

        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for doc_id in candidate_ids {
            if doc_store.get(doc_id)?.is_none() {
                return Err(StorageBackendError::Other(format!(
                    "facet candidate {doc_id} is missing from the document store"
                )));
            }
            if let Some(v) = doc_store.get_field(doc_id, &self.field)? {
                let key = value_to_string(&v);
                let count = counts.entry(key).or_insert(0);
                *count = count.checked_add(1).ok_or_else(|| {
                    StorageBackendError::Other("facet count overflowed u64".to_string())
                })?;
            }
        }

        let mut entries = Vec::with_capacity(counts.len());
        for (i, (value, count)) in counts.into_iter().enumerate() {
            if count > 9_007_199_254_740_992 {
                return Err(StorageBackendError::Other(format!(
                    "facet count {count} cannot be represented exactly as an f64 score"
                )));
            }
            let mut fields = BTreeMap::new();
            fields.insert("_facet_field".to_string(), Value::Str(self.field.clone()));
            fields.insert("_facet_value".to_string(), Value::Str(value));
            fields.insert(
                "_facet_count".to_string(),
                Value::Int(i64::try_from(count).map_err(|_| {
                    StorageBackendError::Other(format!(
                        "facet count {count} exceeds the Value::Int range"
                    ))
                })?),
            );
            entries.push(PostingEntry::new(
                DocId::try_from(i).map_err(|_| {
                    StorageBackendError::Other(format!(
                        "facet bucket index {i} exceeds the document-id range"
                    ))
                })?,
                Payload {
                    positions: Vec::new(),
                    score: count as f64,
                    fields,
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// `Score_q`: apply a [`Scorer`] to every entry of a source posting list.
/// IDF and per-document length are hoisted out of the inner loop.
pub struct ScoreOperator {
    pub scorer: Arc<dyn Scorer>,
    pub source: Arc<dyn Operator>,
    pub query_terms: Vec<String>,
    pub field: FieldName,
}

impl ScoreOperator {
    pub fn new(
        scorer: Arc<dyn Scorer>,
        source: Arc<dyn Operator>,
        query_terms: Vec<String>,
        field: impl Into<FieldName>,
    ) -> Self {
        Self {
            scorer,
            source,
            query_terms,
            field: field.into(),
        }
    }
}

impl Operator for ScoreOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let source_pl = self.source.execute(ctx)?;
        let Some(idx) = ctx.inverted_index.as_ref() else {
            return Err(missing_backend("inverted-index", "score operator"));
        };

        // Pre-compute per-term IDF.
        let mut term_idfs = Vec::with_capacity(self.query_terms.len());
        for term in &self.query_terms {
            term_idfs.push(self.scorer.idf(idx.doc_freq(&self.field, term)?));
        }

        let doc_ids: Vec<DocId> = source_pl.iter().map(|entry| entry.doc_id).collect();
        let scoring_inputs =
            idx.get_scoring_inputs_bulk(&doc_ids, &self.field, &self.query_terms)?;
        if source_pl.len() != scoring_inputs.len() {
            return Err(StorageBackendError::Other(format!(
                "score operator received {} storage inputs for {} source documents",
                scoring_inputs.len(),
                source_pl.len()
            )));
        }
        let mut entries = Vec::with_capacity(source_pl.len());
        let mut per_term_scores = Vec::with_capacity(self.query_terms.len());
        for (entry, (doc_length, term_freqs)) in source_pl.iter().zip(scoring_inputs) {
            per_term_scores.clear();
            per_term_scores.extend(term_freqs.into_iter().zip(&term_idfs).map(
                |(term_freq, idf)| self.scorer.term_score_with_idf(term_freq, doc_length, *idf),
            ));
            let total = self.scorer.finalize_score(&per_term_scores);
            require_finite_score(total, "score operator")?;
            entries.push(PostingEntry {
                doc_id: entry.doc_id,
                payload: Payload {
                    positions: entry.payload.positions.clone(),
                    score: total,
                    fields: entry.payload.fields.clone(),
                },
            });
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }
}
