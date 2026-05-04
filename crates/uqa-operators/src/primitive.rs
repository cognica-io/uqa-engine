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

use crate::base::{ExecutionContext, Operator};

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
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let Some(idx) = ctx.inverted_index.as_ref() else {
            return PostingList::new();
        };
        let tokens = idx.analyzer().analyze(&self.term);
        if tokens.is_empty() {
            return PostingList::new();
        }
        let mut acc = idx.get_posting_list(&self.field, &tokens[0]);
        for t in &tokens[1..] {
            acc = acc.union(&idx.get_posting_list(&self.field, t));
        }
        acc
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        stats.doc_freq(&self.field, &self.term) as f64
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
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return PostingList::new();
        };
        let null_aware = self.predicate.is_null_aware();

        let candidates: Vec<PostingEntry> = if let Some(src) = &self.source {
            src.execute(ctx).into_iter().collect()
        } else {
            doc_store
                .doc_ids()
                .into_iter()
                .map(|id| PostingEntry::new(id, Payload::default()))
                .collect()
        };

        let mut out = Vec::with_capacity(candidates.len());
        for entry in candidates {
            let value = doc_store.get_field(entry.doc_id, &self.field);
            let matched = if null_aware {
                self.predicate.evaluate(value.as_ref())
            } else {
                value.is_some() && self.predicate.evaluate(value.as_ref())
            };
            if matched {
                out.push(entry);
            }
        }
        PostingList::from_sorted_unchecked(out)
    }
}

/// `Facet_f`: count distinct values of a field over a source posting list
/// (or the entire document store). The result is a posting list whose
/// `payload.fields` carry `_facet_field`, `_facet_value`, `_facet_count`,
/// matching the Python encoding.
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
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return PostingList::new();
        };

        let candidate_ids: Vec<DocId> = if let Some(src) = &self.source {
            src.execute(ctx).doc_ids().collect()
        } else {
            doc_store.doc_ids()
        };

        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for doc_id in candidate_ids {
            if let Some(v) = doc_store.get_field(doc_id, &self.field) {
                let key = value_to_string(&v);
                *counts.entry(key).or_insert(0) += 1;
            }
        }

        let mut entries = Vec::with_capacity(counts.len());
        for (i, (value, count)) in counts.into_iter().enumerate() {
            let mut fields = BTreeMap::new();
            fields.insert("_facet_field".to_string(), Value::Str(self.field.clone()));
            fields.insert("_facet_value".to_string(), Value::Str(value));
            fields.insert("_facet_count".to_string(), Value::Int(count as i64));
            entries.push(PostingEntry::new(
                i as DocId,
                Payload {
                    positions: Vec::new(),
                    score: count as f64,
                    fields,
                },
            ));
        }
        PostingList::from_sorted_unchecked(entries)
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
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let source_pl = self.source.execute(ctx);
        let Some(idx) = ctx.inverted_index.as_ref() else {
            return source_pl;
        };

        // Pre-compute per-term IDF.
        let term_idfs: Vec<f64> = self
            .query_terms
            .iter()
            .map(|t| {
                let df = idx.doc_freq(&self.field, t);
                self.scorer.idf(df)
            })
            .collect();

        let entries: Vec<PostingEntry> = source_pl
            .iter()
            .map(|entry| {
                let dl = idx.get_doc_length(entry.doc_id, &self.field);
                let per_term: Vec<f64> = self
                    .query_terms
                    .iter()
                    .zip(&term_idfs)
                    .map(|(term, idf)| {
                        let tf = idx.get_term_freq(entry.doc_id, &self.field, term);
                        self.scorer.score_with_idf(tf, dl, *idf)
                    })
                    .collect();
                let total = self.scorer.combine_scores(&per_term);
                PostingEntry {
                    doc_id: entry.doc_id,
                    payload: Payload {
                        positions: entry.payload.positions.clone(),
                        score: total,
                        fields: entry.payload.fields.clone(),
                    },
                }
            })
            .collect();
        PostingList::from_sorted_unchecked(entries)
    }
}
