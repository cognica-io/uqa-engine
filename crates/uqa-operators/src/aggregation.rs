//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregation monoids and posting-list aggregate operators.
//!
//! Aggregation functions
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
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::base::{missing_backend, ExecutionContext, Operator, OperatorResult};

/// Pair-wise additive state used by [`AvgMonoid`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AvgState {
    pub sum: f64,
    pub count: u64,
}

/// Generic aggregation state. Concrete monoids consume / emit the
/// variants they care about; a mismatched variant is an execution
/// error rather than a silently accepted partial fold.
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
    fn accumulate(&self, state: AggState, value: &Value) -> StorageBackendResult<AggState>;
    fn combine(&self, a: AggState, b: AggState) -> StorageBackendResult<AggState>;
    fn finalize(&self, state: AggState) -> StorageBackendResult<Value>;
}

fn invalid_state(operation: &str, expected: &str, actual: &AggState) -> StorageBackendError {
    StorageBackendError::Other(format!(
        "{operation} aggregation expected {expected} state, got {actual:?}"
    ))
}

fn numeric_value(operation: &str, value: &Value) -> StorageBackendResult<Option<f64>> {
    let numeric = match value {
        Value::Null => return Ok(None),
        Value::Int(integer) => *integer as f64,
        Value::Float(float) => *float,
        Value::Bool(true) => 1.0,
        Value::Bool(false) => 0.0,
        _ => {
            return Err(StorageBackendError::Other(format!(
                "{operation} aggregation requires a numeric value, got {value:?}"
            )))
        }
    };
    if !numeric.is_finite() {
        return Err(StorageBackendError::Other(format!(
            "{operation} aggregation requires a finite numeric value, got {numeric}"
        )));
    }
    Ok(Some(numeric))
}

fn finite_result(operation: &str, value: f64) -> StorageBackendResult<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(StorageBackendError::Other(format!(
            "{operation} aggregation overflowed the finite numeric range"
        )))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CountMonoid;

impl AggregationMonoid for CountMonoid {
    fn identity(&self) -> AggState {
        AggState::Count(0)
    }
    fn accumulate(&self, state: AggState, _value: &Value) -> StorageBackendResult<AggState> {
        let AggState::Count(count) = state else {
            return Err(invalid_state("count", "Count", &state));
        };
        Ok(AggState::Count(count.checked_add(1).ok_or_else(|| {
            StorageBackendError::Other("count aggregation overflowed u64".to_string())
        })?))
    }
    fn combine(&self, a: AggState, b: AggState) -> StorageBackendResult<AggState> {
        let (AggState::Count(left), AggState::Count(right)) = (&a, &b) else {
            return Err(StorageBackendError::Other(format!(
                "count aggregation expected Count states, got {a:?} and {b:?}"
            )));
        };
        Ok(AggState::Count(left.checked_add(*right).ok_or_else(
            || StorageBackendError::Other("count aggregation overflowed u64".to_string()),
        )?))
    }
    fn finalize(&self, state: AggState) -> StorageBackendResult<Value> {
        let AggState::Count(count) = state else {
            return Err(invalid_state("count", "Count", &state));
        };
        let count = i64::try_from(count).map_err(|_| {
            StorageBackendError::Other(format!(
                "count aggregation result {count} exceeds the Value::Int range"
            ))
        })?;
        Ok(Value::Int(count))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SumMonoid;

impl AggregationMonoid for SumMonoid {
    fn identity(&self) -> AggState {
        AggState::Sum(0.0)
    }
    fn accumulate(&self, state: AggState, value: &Value) -> StorageBackendResult<AggState> {
        let Some(delta) = numeric_value("sum", value)? else {
            return Ok(state);
        };
        let AggState::Sum(sum) = state else {
            return Err(invalid_state("sum", "Sum", &state));
        };
        Ok(AggState::Sum(finite_result("sum", sum + delta)?))
    }
    fn combine(&self, a: AggState, b: AggState) -> StorageBackendResult<AggState> {
        let (AggState::Sum(left), AggState::Sum(right)) = (&a, &b) else {
            return Err(StorageBackendError::Other(format!(
                "sum aggregation expected Sum states, got {a:?} and {b:?}"
            )));
        };
        Ok(AggState::Sum(finite_result("sum", left + right)?))
    }
    fn finalize(&self, state: AggState) -> StorageBackendResult<Value> {
        let AggState::Sum(sum) = state else {
            return Err(invalid_state("sum", "Sum", &state));
        };
        Ok(Value::Float(finite_result("sum", sum)?))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AvgMonoid;

impl AggregationMonoid for AvgMonoid {
    fn identity(&self) -> AggState {
        AggState::Avg(AvgState::default())
    }
    fn accumulate(&self, state: AggState, value: &Value) -> StorageBackendResult<AggState> {
        let Some(delta) = numeric_value("avg", value)? else {
            return Ok(state);
        };
        let AggState::Avg(average) = state else {
            return Err(invalid_state("avg", "Avg", &state));
        };
        Ok(AggState::Avg(AvgState {
            sum: finite_result("avg", average.sum + delta)?,
            count: average.count.checked_add(1).ok_or_else(|| {
                StorageBackendError::Other("avg aggregation count overflowed u64".to_string())
            })?,
        }))
    }
    fn combine(&self, a: AggState, b: AggState) -> StorageBackendResult<AggState> {
        let (AggState::Avg(left), AggState::Avg(right)) = (&a, &b) else {
            return Err(StorageBackendError::Other(format!(
                "avg aggregation expected Avg states, got {a:?} and {b:?}"
            )));
        };
        Ok(AggState::Avg(AvgState {
            sum: finite_result("avg", left.sum + right.sum)?,
            count: left.count.checked_add(right.count).ok_or_else(|| {
                StorageBackendError::Other("avg aggregation count overflowed u64".to_string())
            })?,
        }))
    }
    fn finalize(&self, state: AggState) -> StorageBackendResult<Value> {
        let AggState::Avg(average) = state else {
            return Err(invalid_state("avg", "Avg", &state));
        };
        if average.count == 0 {
            Ok(Value::Float(0.0))
        } else {
            Ok(Value::Float(finite_result(
                "avg",
                average.sum / average.count as f64,
            )?))
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MinMonoid;

impl AggregationMonoid for MinMonoid {
    fn identity(&self) -> AggState {
        AggState::Min(f64::INFINITY)
    }
    fn accumulate(&self, state: AggState, value: &Value) -> StorageBackendResult<AggState> {
        let Some(value) = numeric_value("min", value)? else {
            return Ok(state);
        };
        let AggState::Min(minimum) = state else {
            return Err(invalid_state("min", "Min", &state));
        };
        Ok(AggState::Min(minimum.min(value)))
    }
    fn combine(&self, a: AggState, b: AggState) -> StorageBackendResult<AggState> {
        let (AggState::Min(left), AggState::Min(right)) = (&a, &b) else {
            return Err(StorageBackendError::Other(format!(
                "min aggregation expected Min states, got {a:?} and {b:?}"
            )));
        };
        Ok(AggState::Min(left.min(*right)))
    }
    fn finalize(&self, state: AggState) -> StorageBackendResult<Value> {
        let AggState::Min(minimum) = state else {
            return Err(invalid_state("min", "Min", &state));
        };
        if minimum == f64::INFINITY {
            Ok(Value::Null)
        } else {
            Ok(Value::Float(finite_result("min", minimum)?))
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MaxMonoid;

impl AggregationMonoid for MaxMonoid {
    fn identity(&self) -> AggState {
        AggState::Max(f64::NEG_INFINITY)
    }
    fn accumulate(&self, state: AggState, value: &Value) -> StorageBackendResult<AggState> {
        let Some(value) = numeric_value("max", value)? else {
            return Ok(state);
        };
        let AggState::Max(maximum) = state else {
            return Err(invalid_state("max", "Max", &state));
        };
        Ok(AggState::Max(maximum.max(value)))
    }
    fn combine(&self, a: AggState, b: AggState) -> StorageBackendResult<AggState> {
        let (AggState::Max(left), AggState::Max(right)) = (&a, &b) else {
            return Err(StorageBackendError::Other(format!(
                "max aggregation expected Max states, got {a:?} and {b:?}"
            )));
        };
        Ok(AggState::Max(left.max(*right)))
    }
    fn finalize(&self, state: AggState) -> StorageBackendResult<Value> {
        let AggState::Max(maximum) = state else {
            return Err(invalid_state("max", "Max", &state));
        };
        if maximum == f64::NEG_INFINITY {
            Ok(Value::Null)
        } else {
            Ok(Value::Float(finite_result("max", maximum)?))
        }
    }
}

/// Quantile aggregation: collects observed values and computes the
/// requested quantile at finalize. `quantile = 0.5` is the median.
#[derive(Debug, Clone, Copy)]
pub struct QuantileMonoid {
    quantile: f64,
}

impl QuantileMonoid {
    pub fn new(quantile: f64) -> StorageBackendResult<Self> {
        if !quantile.is_finite() || !(0.0..=1.0).contains(&quantile) {
            return Err(StorageBackendError::Other(format!(
                "quantile must be finite and in [0, 1], got {quantile}"
            )));
        }
        Ok(Self { quantile })
    }
}

impl AggregationMonoid for QuantileMonoid {
    fn identity(&self) -> AggState {
        AggState::Values(Vec::new())
    }
    fn accumulate(&self, state: AggState, value: &Value) -> StorageBackendResult<AggState> {
        let Some(value) = numeric_value("quantile", value)? else {
            return Ok(state);
        };
        let AggState::Values(mut values) = state else {
            return Err(invalid_state("quantile", "Values", &state));
        };
        values.push(value);
        Ok(AggState::Values(values))
    }
    fn combine(&self, a: AggState, b: AggState) -> StorageBackendResult<AggState> {
        let (AggState::Values(mut left), AggState::Values(right)) = (a, b) else {
            return Err(StorageBackendError::Other(
                "quantile aggregation expected Values states".to_string(),
            ));
        };
        left.extend(right);
        Ok(AggState::Values(left))
    }
    fn finalize(&self, state: AggState) -> StorageBackendResult<Value> {
        let mut buf = match state {
            AggState::Values(v) => v,
            other => return Err(invalid_state("quantile", "Values", &other)),
        };
        if buf.is_empty() {
            return Ok(Value::Null);
        }
        if buf.iter().any(|value| !value.is_finite()) {
            return Err(StorageBackendError::Other(
                "quantile aggregation state contains a non-finite value".to_string(),
            ));
        }
        buf.sort_by(f64::total_cmp);
        let n = buf.len();
        let idx = self.quantile * (n - 1) as f64;
        let lower = idx.floor() as usize;
        let upper = (lower + 1).min(n - 1);
        let frac = idx - lower as f64;
        Ok(Value::Float(buf[lower] * (1.0 - frac) + buf[upper] * frac))
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let doc_ids: Vec<u64> = if let Some(op) = &self.source {
            op.execute(ctx)?.iter().map(|e| e.doc_id).collect()
        } else {
            let store = ctx
                .document_store
                .as_ref()
                .ok_or_else(|| missing_backend("document-store", "field aggregation"))?;
            let mut ids = store.doc_ids()?;
            ids.sort_unstable();
            ids
        };

        let mut state = self.monoid.identity();
        let store = ctx
            .document_store
            .as_ref()
            .ok_or_else(|| missing_backend("document-store", "field aggregation"))?;
        for doc_id in doc_ids {
            if store.get(doc_id)?.is_none() {
                return Err(StorageBackendError::Other(format!(
                    "field aggregate candidate {doc_id} is missing from the document store"
                )));
            }
            if let Some(value) = store.get_field(doc_id, &self.field)? {
                state = self.monoid.accumulate(state, &value)?;
            }
        }

        let result = self.monoid.finalize(state)?;
        let score = match &result {
            Value::Int(i) => *i as f64,
            Value::Float(f) => *f,
            _ => 0.0,
        };
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        fields.insert("_aggregate_field".into(), Value::Str(self.field.clone()));
        fields.insert("_aggregate".into(), result);
        Ok(PostingList::from_sorted_unchecked(vec![PostingEntry::new(
            0,
            Payload {
                score,
                fields,
                ..Default::default()
            },
        )]))
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
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        let source_pl = self.source.execute(ctx)?;
        let store = ctx
            .document_store
            .as_ref()
            .ok_or_else(|| missing_backend("document-store", "group-by aggregation"))?;

        let mut groups: BTreeMap<String, AggState> = BTreeMap::new();
        for entry in source_pl.iter() {
            if store.get(entry.doc_id)?.is_none() {
                return Err(StorageBackendError::Other(format!(
                    "group-by candidate {} is missing from the document store",
                    entry.doc_id
                )));
            }
            let Some(group_val) = store.get_field(entry.doc_id, &self.group_field)? else {
                continue;
            };
            let key = value_to_key(&group_val);
            let state = groups.entry(key).or_insert_with(|| self.monoid.identity());
            if let Some(agg_val) = store.get_field(entry.doc_id, &self.agg_field)? {
                let new_state = self
                    .monoid
                    .accumulate(std::mem::replace(state, self.monoid.identity()), &agg_val)?;
                *state = new_state;
            }
        }

        let mut entries: Vec<PostingEntry> = Vec::with_capacity(groups.len());
        for (i, (group_key, state)) in groups.into_iter().enumerate() {
            let result = self.monoid.finalize(state)?;
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
                u64::try_from(i).map_err(|_| {
                    StorageBackendError::Other(format!(
                        "group-by bucket index {i} exceeds the document-id range"
                    ))
                })?,
                Payload {
                    score,
                    fields,
                    ..Default::default()
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
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
        a = m.accumulate(a, &Value::Null).unwrap();
        a = m.accumulate(a, &Value::Null).unwrap();
        b = m.accumulate(b, &Value::Null).unwrap();
        let merged = m.combine(a, b).unwrap();
        assert_eq!(m.finalize(merged).unwrap(), Value::Int(3));
    }

    #[test]
    fn sum_monoid_rejects_non_numeric() {
        let m = SumMonoid;
        let mut s = m.identity();
        s = m.accumulate(s, &Value::Float(1.5)).unwrap();
        s = m.accumulate(s, &Value::Int(2)).unwrap();
        assert_eq!(m.finalize(s).unwrap(), Value::Float(3.5));

        let error = m
            .accumulate(m.identity(), &Value::Str("nope".into()))
            .unwrap_err();
        assert!(error.to_string().contains("requires a numeric value"));
    }

    #[test]
    fn avg_monoid_divides_by_count() {
        let m = AvgMonoid;
        let mut s = m.identity();
        for v in [Value::Int(2), Value::Int(4), Value::Int(6)] {
            s = m.accumulate(s, &v).unwrap();
        }
        assert_eq!(m.finalize(s).unwrap(), Value::Float(4.0));
    }

    #[test]
    fn min_max_track_extremes() {
        let mn = MinMonoid;
        let mx = MaxMonoid;
        let mut a = mn.identity();
        let mut b = mx.identity();
        for v in [Value::Float(3.0), Value::Float(1.0), Value::Float(2.0)] {
            a = mn.accumulate(a, &v).unwrap();
            b = mx.accumulate(b, &v).unwrap();
        }
        assert_eq!(mn.finalize(a).unwrap(), Value::Float(1.0));
        assert_eq!(mx.finalize(b).unwrap(), Value::Float(3.0));
    }

    #[test]
    fn quantile_median_interpolates() {
        let q = QuantileMonoid::new(0.5).unwrap();
        let mut s = q.identity();
        for v in [
            Value::Float(1.0),
            Value::Float(2.0),
            Value::Float(3.0),
            Value::Float(4.0),
        ] {
            s = q.accumulate(s, &v).unwrap();
        }
        // median of [1,2,3,4] interpolates between idx 1 and 2 = (2+3)/2 = 2.5
        assert_eq!(q.finalize(s).unwrap(), Value::Float(2.5));
    }

    #[test]
    fn quantile_constructor_rejects_invalid_values() {
        for quantile in [f64::NAN, -0.1, 1.1] {
            let error = QuantileMonoid::new(quantile).unwrap_err();
            assert!(error.to_string().contains("finite and in [0, 1]"));
        }
    }

    #[test]
    fn aggregation_state_mismatches_are_errors() {
        let count = CountMonoid;
        assert!(count
            .accumulate(AggState::Sum(0.0), &Value::Int(1))
            .unwrap_err()
            .to_string()
            .contains("expected Count state"));
        assert!(count
            .combine(AggState::Count(1), AggState::Sum(2.0))
            .unwrap_err()
            .to_string()
            .contains("expected Count states"));
        assert!(count
            .finalize(AggState::Values(Vec::new()))
            .unwrap_err()
            .to_string()
            .contains("expected Count state"));
    }

    #[test]
    fn aggregation_counters_and_value_widths_are_checked() {
        let count = CountMonoid;
        assert!(count
            .accumulate(AggState::Count(u64::MAX), &Value::Null)
            .unwrap_err()
            .to_string()
            .contains("overflowed u64"));
        assert!(count
            .combine(AggState::Count(u64::MAX), AggState::Count(1))
            .unwrap_err()
            .to_string()
            .contains("overflowed u64"));
        assert!(count
            .finalize(AggState::Count(i64::MAX as u64 + 1))
            .unwrap_err()
            .to_string()
            .contains("Value::Int range"));

        let avg = AvgMonoid;
        assert!(avg
            .accumulate(
                AggState::Avg(AvgState {
                    sum: 1.0,
                    count: u64::MAX,
                }),
                &Value::Int(1),
            )
            .unwrap_err()
            .to_string()
            .contains("count overflowed"));
    }

    #[test]
    fn numeric_aggregations_reject_invalid_types_and_non_finite_values() {
        for result in [
            AvgMonoid.accumulate(AvgMonoid.identity(), &Value::Str("bad".into())),
            MinMonoid.accumulate(MinMonoid.identity(), &Value::Float(f64::NAN)),
            MaxMonoid.accumulate(MaxMonoid.identity(), &Value::Float(f64::INFINITY)),
        ] {
            assert!(result.is_err());
        }
        assert_eq!(
            MinMonoid.finalize(MinMonoid.identity()).unwrap(),
            Value::Null
        );
        assert_eq!(
            MaxMonoid.finalize(MaxMonoid.identity()).unwrap(),
            Value::Null
        );
        let quantile = QuantileMonoid::new(0.5).unwrap();
        assert_eq!(quantile.finalize(quantile.identity()).unwrap(), Value::Null);
    }
}
