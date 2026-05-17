//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregation monoids and posting-list aggregate operators.
//!
//! Mirrors UQA `operators/aggregation`. Aggregation functions
//! form a monoid (Section 5.1, Paper 1) so a parallel executor can
//! split the input, fold each shard with [`AggregationMonoid::accumulate`],
//! merge the partial states with [`AggregationMonoid::combine`], and
//! emit the final value via [`AggregationMonoid::finalize`].
//!
//! # Concrete monoids
//!
//! * [`CountMonoid`] -- `(0, +)` over `u64`.
//! * [`SumMonoid`] -- `(0.0, +)` over `f64`.
//! * [`AvgMonoid`] -- `((0.0, 0), pair-wise +)` over `(sum, count)`.
//! * [`MinMonoid`], [`MaxMonoid`] -- `(infinity, min)` / `(-infinity, max)`
//!   over `f64`.
//! * [`QuantileMonoid`] -- collect-then-finalize quantile estimator.
//!
//! # Operators
//!
//! * [`AggregateOperator`] folds a posting list with the supplied
//!   monoid and emits a single-entry posting list whose payload
//!   carries `_aggregate_field` / `_aggregate` metadata.
//! * [`GroupByOperator`] groups by a field, folds a second field with
//!   the monoid per group, and emits one entry per group.

#![allow(
    clippy::redundant_field_names,
    clippy::similar_names,
    clippy::manual_let_else,
    clippy::explicit_iter_loop
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{Payload, PostingEntry, PostingList, Value};

use crate::base::{ExecutionContext, Operator};

/// Pair-wise additive state used by [`AvgMonoid`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AvgState {
    pub sum: f64,
    pub count: u64,
}

/// Generic aggregation state. Concrete monoids consume / emit the
/// variants they care about; mismatched variants surface as
/// `accumulate` / `combine` no-ops so a partial fold never panics.
#[derive(Debug, Clone, PartialEq)]
pub enum AggState {
    Count(u64),
    Sum(f64),
    Avg(AvgState),
    Min(f64),
    Max(f64),
    Values(Vec<f64>),
}

impl AggState {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            AggState::Count(n) => Some(*n as f64),
            AggState::Sum(v) | AggState::Min(v) | AggState::Max(v) => Some(*v),
            AggState::Avg(s) => {
                if s.count == 0 {
                    Some(0.0)
                } else {
                    Some(s.sum / s.count as f64)
                }
            }
            AggState::Values(_) => None,
        }
    }
}

/// Aggregation function with monoid structure for parallel
/// decomposition. See Section 5.1, Paper 1.
pub trait AggregationMonoid: Send + Sync {
    fn identity(&self) -> AggState;
    fn accumulate(&self, state: AggState, value: &Value) -> AggState;
    fn combine(&self, a: AggState, b: AggState) -> AggState;
    fn finalize(&self, state: AggState) -> Value;
}

fn coerce_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(true) => Some(1.0),
        Value::Bool(false) => Some(0.0),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CountMonoid;

impl AggregationMonoid for CountMonoid {
    fn identity(&self) -> AggState {
        AggState::Count(0)
    }
    fn accumulate(&self, state: AggState, _value: &Value) -> AggState {
        match state {
            AggState::Count(n) => AggState::Count(n + 1),
            other => other,
        }
    }
    fn combine(&self, a: AggState, b: AggState) -> AggState {
        match (a, b) {
            (AggState::Count(x), AggState::Count(y)) => AggState::Count(x + y),
            (other, _) => other,
        }
    }
    fn finalize(&self, state: AggState) -> Value {
        match state {
            AggState::Count(n) => Value::Int(n as i64),
            _ => Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SumMonoid;

impl AggregationMonoid for SumMonoid {
    fn identity(&self) -> AggState {
        AggState::Sum(0.0)
    }
    fn accumulate(&self, state: AggState, value: &Value) -> AggState {
        let delta = coerce_f64(value).unwrap_or(0.0);
        match state {
            AggState::Sum(s) => AggState::Sum(s + delta),
            other => other,
        }
    }
    fn combine(&self, a: AggState, b: AggState) -> AggState {
        match (a, b) {
            (AggState::Sum(x), AggState::Sum(y)) => AggState::Sum(x + y),
            (other, _) => other,
        }
    }
    fn finalize(&self, state: AggState) -> Value {
        match state {
            AggState::Sum(s) => Value::Float(s),
            _ => Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AvgMonoid;

impl AggregationMonoid for AvgMonoid {
    fn identity(&self) -> AggState {
        AggState::Avg(AvgState::default())
    }
    fn accumulate(&self, state: AggState, value: &Value) -> AggState {
        let delta = match coerce_f64(value) {
            Some(v) => v,
            None => return state,
        };
        match state {
            AggState::Avg(s) => AggState::Avg(AvgState {
                sum: s.sum + delta,
                count: s.count + 1,
            }),
            other => other,
        }
    }
    fn combine(&self, a: AggState, b: AggState) -> AggState {
        match (a, b) {
            (AggState::Avg(x), AggState::Avg(y)) => AggState::Avg(AvgState {
                sum: x.sum + y.sum,
                count: x.count + y.count,
            }),
            (other, _) => other,
        }
    }
    fn finalize(&self, state: AggState) -> Value {
        match state {
            AggState::Avg(s) => {
                if s.count == 0 {
                    Value::Float(0.0)
                } else {
                    Value::Float(s.sum / s.count as f64)
                }
            }
            _ => Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MinMonoid;

impl AggregationMonoid for MinMonoid {
    fn identity(&self) -> AggState {
        AggState::Min(f64::INFINITY)
    }
    fn accumulate(&self, state: AggState, value: &Value) -> AggState {
        let v = match coerce_f64(value) {
            Some(v) => v,
            None => return state,
        };
        match state {
            AggState::Min(m) => AggState::Min(m.min(v)),
            other => other,
        }
    }
    fn combine(&self, a: AggState, b: AggState) -> AggState {
        match (a, b) {
            (AggState::Min(x), AggState::Min(y)) => AggState::Min(x.min(y)),
            (other, _) => other,
        }
    }
    fn finalize(&self, state: AggState) -> Value {
        match state {
            AggState::Min(m) => Value::Float(m),
            _ => Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MaxMonoid;

impl AggregationMonoid for MaxMonoid {
    fn identity(&self) -> AggState {
        AggState::Max(f64::NEG_INFINITY)
    }
    fn accumulate(&self, state: AggState, value: &Value) -> AggState {
        let v = match coerce_f64(value) {
            Some(v) => v,
            None => return state,
        };
        match state {
            AggState::Max(m) => AggState::Max(m.max(v)),
            other => other,
        }
    }
    fn combine(&self, a: AggState, b: AggState) -> AggState {
        match (a, b) {
            (AggState::Max(x), AggState::Max(y)) => AggState::Max(x.max(y)),
            (other, _) => other,
        }
    }
    fn finalize(&self, state: AggState) -> Value {
        match state {
            AggState::Max(m) => Value::Float(m),
            _ => Value::Null,
        }
    }
}

/// Quantile aggregation: collects observed values and computes the
/// requested quantile at finalize. `quantile = 0.5` is the median.
#[derive(Debug, Clone, Copy)]
pub struct QuantileMonoid {
    pub quantile: f64,
}

impl QuantileMonoid {
    pub fn new(quantile: f64) -> Self {
        assert!(
            (0.0..=1.0).contains(&quantile),
            "quantile must be in [0, 1], got {quantile}"
        );
        Self { quantile }
    }
}

impl AggregationMonoid for QuantileMonoid {
    fn identity(&self) -> AggState {
        AggState::Values(Vec::new())
    }
    fn accumulate(&self, state: AggState, value: &Value) -> AggState {
        let v = match coerce_f64(value) {
            Some(v) => v,
            None => return state,
        };
        match state {
            AggState::Values(mut buf) => {
                buf.push(v);
                AggState::Values(buf)
            }
            other => other,
        }
    }
    fn combine(&self, a: AggState, b: AggState) -> AggState {
        match (a, b) {
            (AggState::Values(mut x), AggState::Values(y)) => {
                x.extend(y);
                AggState::Values(x)
            }
            (other, _) => other,
        }
    }
    fn finalize(&self, state: AggState) -> Value {
        let mut buf = match state {
            AggState::Values(v) => v,
            _ => return Value::Null,
        };
        if buf.is_empty() {
            return Value::Float(0.0);
        }
        buf.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = buf.len();
        let idx = self.quantile * (n - 1) as f64;
        let lower = idx.floor() as usize;
        let upper = (lower + 1).min(n - 1);
        let frac = idx - lower as f64;
        Value::Float(buf[lower] * (1.0 - frac) + buf[upper] * frac)
    }
}

// -------------------------------------------------------------------------
// Operators
// -------------------------------------------------------------------------

/// Apply a monoid over a single field across the rows the source
/// operator produces. Emits a one-entry posting list whose payload
/// carries `_aggregate_field` / `_aggregate` so downstream callers
/// can pick the result by name.
pub struct AggregateOperator {
    pub source: Option<Arc<dyn Operator>>,
    pub field: String,
    pub monoid: Arc<dyn AggregationMonoid>,
}

impl AggregateOperator {
    pub fn new(
        source: Option<Arc<dyn Operator>>,
        field: impl Into<String>,
        monoid: Arc<dyn AggregationMonoid>,
    ) -> Self {
        Self {
            source,
            field: field.into(),
            monoid,
        }
    }
}

impl Operator for AggregateOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let doc_ids: Vec<u64> = match &self.source {
            Some(op) => op.execute(ctx).iter().map(|e| e.doc_id).collect(),
            None => match ctx.document_store.as_ref() {
                Some(store) => {
                    let mut ids = store.doc_ids();
                    ids.sort_unstable();
                    ids
                }
                None => Vec::new(),
            },
        };

        let mut state = self.monoid.identity();
        if let Some(store) = ctx.document_store.as_ref() {
            for doc_id in doc_ids {
                if let Some(value) = store.get_field(doc_id, &self.field) {
                    state = self.monoid.accumulate(state, &value);
                }
            }
        }

        let result = self.monoid.finalize(state);
        let score = match &result {
            Value::Int(i) => *i as f64,
            Value::Float(f) => *f,
            _ => 0.0,
        };
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        fields.insert("_aggregate_field".into(), Value::Str(self.field.clone()));
        fields.insert("_aggregate".into(), result);
        PostingList::from_sorted_unchecked(vec![PostingEntry::new(
            0,
            Payload {
                score,
                fields,
                ..Default::default()
            },
        )])
    }
}

/// Group documents by `group_field` and fold `agg_field` per group.
pub struct GroupByOperator {
    pub source: Arc<dyn Operator>,
    pub group_field: String,
    pub agg_field: String,
    pub monoid: Arc<dyn AggregationMonoid>,
}

impl GroupByOperator {
    pub fn new(
        source: Arc<dyn Operator>,
        group_field: impl Into<String>,
        agg_field: impl Into<String>,
        monoid: Arc<dyn AggregationMonoid>,
    ) -> Self {
        Self {
            source,
            group_field: group_field.into(),
            agg_field: agg_field.into(),
            monoid,
        }
    }
}

impl Operator for GroupByOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let source_pl = self.source.execute(ctx);
        let store = match ctx.document_store.as_ref() {
            Some(s) => s,
            None => return PostingList::default(),
        };

        let mut groups: BTreeMap<String, AggState> = BTreeMap::new();
        for entry in source_pl.iter() {
            let Some(group_val) = store.get_field(entry.doc_id, &self.group_field) else {
                continue;
            };
            let key = value_to_key(&group_val);
            let state = groups.entry(key).or_insert_with(|| self.monoid.identity());
            if let Some(agg_val) = store.get_field(entry.doc_id, &self.agg_field) {
                let new_state = self
                    .monoid
                    .accumulate(std::mem::replace(state, self.monoid.identity()), &agg_val);
                *state = new_state;
            }
        }

        let mut entries: Vec<PostingEntry> = Vec::with_capacity(groups.len());
        for (i, (group_key, state)) in groups.into_iter().enumerate() {
            let result = self.monoid.finalize(state);
            let score = match &result {
                Value::Int(i) => *i as f64,
                Value::Float(f) => *f,
                _ => 0.0,
            };
            let mut fields: BTreeMap<String, Value> = BTreeMap::new();
            fields.insert("_group_key".into(), Value::Str(group_key));
            fields.insert("_group_field".into(), Value::Str(self.group_field.clone()));
            fields.insert("_aggregate_result".into(), result);
            entries.push(PostingEntry::new(
                i as u64,
                Payload {
                    score,
                    fields,
                    ..Default::default()
                },
            ));
        }
        PostingList::from_sorted_unchecked(entries)
    }
}

fn value_to_key(v: &Value) -> String {
    match v {
        Value::Null => "\x00".into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{f:.17}"),
        Value::Str(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_monoid_combines_partial_folds() {
        let m = CountMonoid;
        let mut a = m.identity();
        let mut b = m.identity();
        a = m.accumulate(a, &Value::Null);
        a = m.accumulate(a, &Value::Null);
        b = m.accumulate(b, &Value::Null);
        let merged = m.combine(a, b);
        assert_eq!(m.finalize(merged), Value::Int(3));
    }

    #[test]
    fn sum_monoid_skips_non_numeric() {
        let m = SumMonoid;
        let mut s = m.identity();
        s = m.accumulate(s, &Value::Float(1.5));
        s = m.accumulate(s, &Value::Int(2));
        s = m.accumulate(s, &Value::Str("nope".into()));
        assert_eq!(m.finalize(s), Value::Float(3.5));
    }

    #[test]
    fn avg_monoid_divides_by_count() {
        let m = AvgMonoid;
        let mut s = m.identity();
        for v in [Value::Int(2), Value::Int(4), Value::Int(6)] {
            s = m.accumulate(s, &v);
        }
        assert_eq!(m.finalize(s), Value::Float(4.0));
    }

    #[test]
    fn min_max_track_extremes() {
        let mn = MinMonoid;
        let mx = MaxMonoid;
        let mut a = mn.identity();
        let mut b = mx.identity();
        for v in [Value::Float(3.0), Value::Float(1.0), Value::Float(2.0)] {
            a = mn.accumulate(a, &v);
            b = mx.accumulate(b, &v);
        }
        assert_eq!(mn.finalize(a), Value::Float(1.0));
        assert_eq!(mx.finalize(b), Value::Float(3.0));
    }

    #[test]
    fn quantile_median_interpolates() {
        let q = QuantileMonoid::new(0.5);
        let mut s = q.identity();
        for v in [
            Value::Float(1.0),
            Value::Float(2.0),
            Value::Float(3.0),
            Value::Float(4.0),
        ] {
            s = q.accumulate(s, &v);
        }
        // median of [1,2,3,4] interpolates between idx 1 and 2 = (2+3)/2 = 2.5
        assert_eq!(q.finalize(s), Value::Float(2.5));
    }
}
