//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hierarchical / nested-document operators (Definitions 5.3.1-5.3.5,
//! Paper 1).
//!
//! `PathSegment` traversal works against the same document shape the
//! engine already uses: a `Document` is a `BTreeMap<String, Value>`,
//! and `Value::Map` / `Value::List` thread through the path.
//!
//! Paths are dotted strings: `metadata.author` or `orders.0.amount`.
//! A bare segment (no digit) selects a map key; a numeric segment
//! indexes into a list.

use std::sync::Arc;

use uqa_core::{
    IndexStats, PathExpr, PathSegment, Payload, PostingEntry, PostingList, Predicate, Value,
};
use uqa_storage::document_store::Document;

use crate::base::{ExecutionContext, Operator};
use crate::primitive::FilterOperator;

/// Parse a dotted path: each numeric segment becomes an index, every
/// other segment a key lookup.
pub fn parse_path(path: &str) -> PathExpr {
    path.split('.')
        .map(|seg| match seg.parse::<usize>() {
            Ok(n) => PathSegment::Index(n),
            Err(_) => PathSegment::Key(seg.to_string()),
        })
        .collect()
}

/// Evaluate a path against a document, descending through `Value::Map`
/// and `Value::List`. Returns `None` if any segment fails to resolve.
pub fn eval_path(doc: &Document, path: &[PathSegment]) -> Option<Value> {
    let mut current: Value = match path.first()? {
        PathSegment::Key(k) => doc.get(k)?.clone(),
        PathSegment::Index(_) => return None,
    };
    for seg in path.iter().skip(1) {
        current = match (current, seg) {
            (Value::Map(m), PathSegment::Key(k)) => m.get(k)?.clone(),
            (Value::List(items), PathSegment::Index(i)) => items.get(*i)?.clone(),
            (Value::List(items), PathSegment::Key(k)) => {
                // Map a key over a list of maps: collect the key from each
                // element so downstream callers see a list at the leaf.
                let collected: Vec<Value> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::Map(m) => m.get(k).cloned(),
                        _ => None,
                    })
                    .collect();
                Value::List(collected)
            }
            _ => return None,
        };
    }
    Some(current)
}

/// Project `paths` out of `doc` into a flat map keyed by the dotted
/// path string. Mirrors `uqa.core.hierarchical.project_paths` --
/// segments stringify in the same way (numeric indices render as
/// their integer literal).
pub fn project_paths(
    doc: &Document,
    paths: &[PathExpr],
) -> std::collections::BTreeMap<String, Value> {
    let mut out = std::collections::BTreeMap::new();
    for path in paths {
        let key = path
            .iter()
            .map(|seg| match seg {
                PathSegment::Key(k) => k.clone(),
                PathSegment::Index(i) => i.to_string(),
            })
            .collect::<Vec<_>>()
            .join(".");
        let value = eval_path(doc, path).unwrap_or(Value::Null);
        out.insert(key, value);
    }
    out
}

/// Unnest an array at `path` into a sequence of synthesised
/// documents. Mirrors `uqa.core.hierarchical.unnest_array`. Each
/// emitted document is a clone of the source merged with two
/// metadata fields:
///
/// * `<path>._unnested` -- the array element value.
/// * `_unnest_index`    -- the element's 0-based index.
pub fn unnest_array(doc: &Document, path: &[PathSegment]) -> Vec<Document> {
    let resolved = eval_path(doc, path);
    let Some(Value::List(items)) = resolved else {
        return Vec::new();
    };
    let path_key = path
        .iter()
        .map(|seg| match seg {
            PathSegment::Key(k) => k.clone(),
            PathSegment::Index(i) => i.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".");
    let unnest_key = format!("{path_key}._unnested");
    items
        .into_iter()
        .enumerate()
        .map(|(idx, item)| {
            let mut nested = doc.clone();
            nested.insert(unnest_key.clone(), item);
            nested.insert("_unnest_index".to_string(), Value::Int(idx as i64));
            nested
        })
        .collect()
}

// -------------------------------------------------------------------------
// PathFilter
// -------------------------------------------------------------------------

/// Filter documents by whether `path`'s value (or any element if the
/// resolved value is a list) matches `predicate`.
pub struct PathFilterOperator {
    pub path: PathExpr,
    pub predicate: Predicate,
    pub source: Option<Arc<dyn Operator>>,
}

impl PathFilterOperator {
    pub fn new(path: PathExpr, predicate: Predicate, source: Option<Arc<dyn Operator>>) -> Self {
        Self {
            path,
            predicate,
            source,
        }
    }
}

impl Operator for PathFilterOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return PostingList::new();
        };
        let candidates: Vec<u64> = match &self.source {
            Some(src) => src.execute(ctx).doc_ids().collect(),
            None => doc_store.doc_ids(),
        };
        let mut entries: Vec<PostingEntry> = Vec::new();
        for doc_id in candidates {
            let Some(doc) = doc_store.get(doc_id) else {
                continue;
            };
            let Some(value) = eval_path(&doc, &self.path) else {
                if self.predicate.is_null_aware() && self.predicate.evaluate(None) {
                    entries.push(PostingEntry::new(doc_id, Payload::default()));
                }
                continue;
            };
            let matched = match &value {
                Value::List(items) => items.iter().any(|v| self.predicate.evaluate(Some(v))),
                other => self.predicate.evaluate(Some(other)),
            };
            if matched {
                entries.push(PostingEntry::new(doc_id, Payload::default()));
            }
        }
        entries.sort_by_key(|e| e.doc_id);
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        match &self.source {
            Some(src) => src.cost_estimate(stats),
            None => stats.total_docs as f64,
        }
    }
}

// -------------------------------------------------------------------------
// PathProject
// -------------------------------------------------------------------------

/// Project documents to the named paths: each path's resolved value
/// lands in the output entry's `payload.fields` keyed on its dotted
/// representation. Documents that fail to resolve a path keep the
/// missing key absent.
pub struct PathProjectOperator {
    pub paths: Vec<PathExpr>,
    pub source: Arc<dyn Operator>,
}

impl PathProjectOperator {
    pub fn new(paths: Vec<PathExpr>, source: Arc<dyn Operator>) -> Self {
        Self { paths, source }
    }
}

impl Operator for PathProjectOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let source_pl = self.source.execute(ctx);
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return source_pl;
        };
        let mut entries: Vec<PostingEntry> = Vec::new();
        for entry in source_pl.entries() {
            let Some(doc) = doc_store.get(entry.doc_id) else {
                continue;
            };
            let mut fields = entry.payload.fields.clone();
            for path in &self.paths {
                if let Some(value) = eval_path(&doc, path) {
                    fields.insert(path_key(path), value);
                }
            }
            entries.push(PostingEntry::new(
                entry.doc_id,
                Payload {
                    positions: entry.payload.positions.clone(),
                    score: entry.payload.score,
                    fields,
                },
            ));
        }
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        self.source.cost_estimate(stats)
    }
}

fn path_key(path: &[PathSegment]) -> String {
    let mut parts = Vec::with_capacity(path.len());
    for seg in path {
        match seg {
            PathSegment::Key(k) => parts.push(k.clone()),
            PathSegment::Index(i) => parts.push(i.to_string()),
        }
    }
    parts.join(".")
}

// -------------------------------------------------------------------------
// PathAggregate
// -------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum AggregationKind {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

/// Aggregate a numeric path across nested arrays per-document. The
/// payload's score becomes the aggregate value; `_path_aggregate` and
/// `_path_aggregate_path` carry the value and the dotted path for
/// downstream consumers.
pub struct PathAggregateOperator {
    pub path: PathExpr,
    pub agg: AggregationKind,
    pub source: Option<Arc<dyn Operator>>,
}

impl PathAggregateOperator {
    pub fn new(path: PathExpr, agg: AggregationKind, source: Option<Arc<dyn Operator>>) -> Self {
        Self { path, agg, source }
    }
}

impl Operator for PathAggregateOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let Some(doc_store) = ctx.document_store.as_ref() else {
            return PostingList::new();
        };
        let candidates: Vec<u64> = match &self.source {
            Some(src) => src.execute(ctx).doc_ids().collect(),
            None => doc_store.doc_ids(),
        };
        let mut entries: Vec<PostingEntry> = Vec::new();
        for doc_id in candidates {
            let Some(doc) = doc_store.get(doc_id) else {
                continue;
            };
            let value = eval_path(&doc, &self.path);
            let mut numeric: Vec<f64> = Vec::new();
            match value {
                Some(Value::List(items)) => {
                    for v in items {
                        if let Some(n) = value_as_f64(&v) {
                            numeric.push(n);
                        }
                    }
                }
                Some(other) => {
                    if let Some(n) = value_as_f64(&other) {
                        numeric.push(n);
                    }
                }
                None => {}
            }
            let result = aggregate(self.agg, &numeric);
            let mut fields = std::collections::BTreeMap::new();
            fields.insert(
                "_path_aggregate_path".into(),
                Value::Str(path_key(&self.path)),
            );
            fields.insert("_path_aggregate".into(), Value::Float(result));
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    positions: Vec::new(),
                    score: result,
                    fields,
                },
            ));
        }
        entries.sort_by_key(|e| e.doc_id);
        PostingList::from_sorted_unchecked(entries)
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        match &self.source {
            Some(src) => src.cost_estimate(stats),
            None => stats.total_docs as f64,
        }
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn aggregate(kind: AggregationKind, values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    match kind {
        AggregationKind::Sum => values.iter().sum(),
        AggregationKind::Avg => values.iter().sum::<f64>() / values.len() as f64,
        AggregationKind::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
        AggregationKind::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        AggregationKind::Count => values.len() as f64,
    }
}

// -------------------------------------------------------------------------
// UnifiedFilter
// -------------------------------------------------------------------------

/// Dispatch between [`FilterOperator`] (flat field) and
/// [`PathFilterOperator`] (dotted path). The decision depends purely
/// on whether `field_expr` contains `.`.
pub struct UnifiedFilterOperator {
    pub field_expr: String,
    pub predicate: Predicate,
    pub source: Option<Arc<dyn Operator>>,
}

impl UnifiedFilterOperator {
    pub fn new(
        field_expr: impl Into<String>,
        predicate: Predicate,
        source: Option<Arc<dyn Operator>>,
    ) -> Self {
        Self {
            field_expr: field_expr.into(),
            predicate,
            source,
        }
    }
}

impl Operator for UnifiedFilterOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        if self.field_expr.contains('.') {
            let path = parse_path(&self.field_expr);
            let inner = PathFilterOperator::new(path, self.predicate.clone(), self.source.clone());
            inner.execute(ctx)
        } else {
            let inner = FilterOperator::new(
                self.field_expr.clone(),
                self.predicate.clone(),
                self.source.clone(),
            );
            inner.execute(ctx)
        }
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        match &self.source {
            Some(src) => src.cost_estimate(stats),
            None => stats.total_docs as f64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dotted_path() {
        let p = parse_path("orders.0.amount");
        assert_eq!(
            p,
            vec![
                PathSegment::Key("orders".into()),
                PathSegment::Index(0),
                PathSegment::Key("amount".into()),
            ]
        );
    }

    #[test]
    fn eval_path_descends_map_then_list_then_key() {
        let mut doc: Document = std::collections::BTreeMap::new();
        let mut order = std::collections::BTreeMap::new();
        order.insert("amount".into(), Value::Int(7));
        doc.insert("orders".into(), Value::List(vec![Value::Map(order)]));
        let v = eval_path(&doc, &parse_path("orders.0.amount")).unwrap();
        assert_eq!(v, Value::Int(7));
    }

    #[test]
    fn eval_path_maps_key_over_list_of_maps() {
        let mut doc: Document = std::collections::BTreeMap::new();
        let mut o1 = std::collections::BTreeMap::new();
        o1.insert("amount".into(), Value::Int(7));
        let mut o2 = std::collections::BTreeMap::new();
        o2.insert("amount".into(), Value::Int(11));
        doc.insert(
            "orders".into(),
            Value::List(vec![Value::Map(o1), Value::Map(o2)]),
        );
        let v = eval_path(&doc, &parse_path("orders.amount")).unwrap();
        assert_eq!(v, Value::List(vec![Value::Int(7), Value::Int(11)]));
    }
}
