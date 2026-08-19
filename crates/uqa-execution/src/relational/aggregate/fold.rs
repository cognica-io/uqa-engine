//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in aggregate state transitions and finalization.

use super::{
    compare_values, eval_scalar, AggregateKind, AggregateSpec, ExecError, ExecResult, SQLParam,
    ScalarEvalContext, ScalarExpr, Value,
};
use crate::PhysicalRow;
use uqa_sql::expr::RowLookup;

pub(super) struct GroupState {
    pub(super) folds: Vec<AggFold>,
    pub(super) key_values: Vec<Value>,
}

pub(in crate::relational) struct AggFold {
    pub(in crate::relational) count: u64,
    pub(super) sum: Option<f64>,
    pub(super) min: Option<Value>,
    pub(super) max: Option<Value>,
    distinct: Option<crate::distinct::SeenKeySet>,
}

impl AggFold {
    pub(in crate::relational) fn new(work_mem_bytes: usize, distinct: bool) -> Self {
        Self {
            count: 0,
            sum: None,
            min: None,
            max: None,
            distinct: distinct.then(|| crate::distinct::SeenKeySet::new(work_mem_bytes, None)),
        }
    }
}

pub(in crate::relational) fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        Value::Bool(true) => Some(1.0),
        Value::Bool(false) => Some(0.0),
        _ => None,
    }
}

pub(super) fn fold_into(
    state: &mut AggFold,
    spec: &AggregateSpec,
    row: &dyn RowLookup,
    params: &[SQLParam],
) -> ExecResult<()> {
    if spec.kind == AggregateKind::CountStar {
        state.count = checked_count_add(state.count, 1)?;
        return Ok(());
    }

    let argument = spec.arg.as_ref().ok_or_else(|| {
        ExecError::Other(format!(
            "aggregate {:?} requires an argument expression",
            spec.kind
        ))
    })?;
    let context = ScalarEvalContext::from_row_lookup(row, params);
    let value = eval_scalar(argument, &context)?;
    if matches!(value, Value::Null) {
        return Ok(());
    }
    if let Some(distinct) = state.distinct.as_mut() {
        let key = crate::distinct::encode_key(std::slice::from_ref(&value))?;
        if !distinct.insert(key)? {
            return Ok(());
        }
    }

    match spec.kind {
        AggregateKind::Count => state.count = checked_count_add(state.count, 1)?,
        AggregateKind::Sum | AggregateKind::Avg => {
            let numeric = value_to_f64(&value).ok_or_else(|| {
                ExecError::Other(format!("non-numeric input to SUM/AVG: {value:?}"))
            })?;
            state.sum = Some(state.sum.unwrap_or(0.0) + numeric);
            state.count = checked_count_add(state.count, 1)?;
        }
        AggregateKind::Min => merge_min(&mut state.min, Some(value)),
        AggregateKind::Max => merge_max(&mut state.max, Some(value)),
        AggregateKind::CountStar => unreachable!("COUNT(*) returned before argument evaluation"),
    }
    Ok(())
}

pub(super) fn merge_fold(target: &mut AggFold, source: AggFold) -> ExecResult<()> {
    if target.distinct.is_some() || source.distinct.is_some() {
        return Err(ExecError::Other(
            "DISTINCT aggregate state cannot be merged from partials".into(),
        ));
    }
    target.count = checked_count_add(target.count, source.count)?;
    target.sum = match (target.sum, source.sum) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    merge_min(&mut target.min, source.min);
    merge_max(&mut target.max, source.max);
    Ok(())
}

fn checked_count_add(left: u64, right: u64) -> ExecResult<u64> {
    left.checked_add(right)
        .ok_or_else(|| ExecError::Other("aggregate count overflow".into()))
}

fn merge_min(target: &mut Option<Value>, source: Option<Value>) {
    if let Some(source) = source {
        if target
            .as_ref()
            .is_none_or(|current| compare_values(&source, current).is_lt())
        {
            *target = Some(source);
        }
    }
}

fn merge_max(target: &mut Option<Value>, source: Option<Value>) {
    if let Some(source) = source {
        if target
            .as_ref()
            .is_none_or(|current| compare_values(&source, current).is_gt())
        {
            *target = Some(source);
        }
    }
}

pub(super) fn finalise_builtin_group(
    state: GroupState,
    group_keys: &[(String, ScalarExpr)],
    aggregates: &[AggregateSpec],
) -> ExecResult<PhysicalRow> {
    if state.key_values.len() != group_keys.len() {
        return Err(ExecError::Other(format!(
            "aggregate group has {} keys, expected {}",
            state.key_values.len(),
            group_keys.len()
        )));
    }
    if state.folds.len() != aggregates.len() {
        return Err(ExecError::Other(format!(
            "aggregate group has {} folds, expected {}",
            state.folds.len(),
            aggregates.len()
        )));
    }
    let mut values = state.key_values;
    for (fold, spec) in state.folds.into_iter().zip(aggregates) {
        values.push(finalise_owned_fold(fold, spec)?);
    }
    Ok(PhysicalRow::from_values(values))
}

#[cfg(test)]
pub(in crate::relational) fn finalise_fold(
    state: &AggFold,
    spec: &AggregateSpec,
) -> ExecResult<Value> {
    finalise_fold_values(
        state.count,
        state.sum,
        state.min.clone(),
        state.max.clone(),
        spec,
    )
}

fn finalise_owned_fold(state: AggFold, spec: &AggregateSpec) -> ExecResult<Value> {
    finalise_fold_values(state.count, state.sum, state.min, state.max, spec)
}

fn finalise_fold_values(
    count: u64,
    sum: Option<f64>,
    min: Option<Value>,
    max: Option<Value>,
    spec: &AggregateSpec,
) -> ExecResult<Value> {
    Ok(match spec.kind {
        AggregateKind::Count | AggregateKind::CountStar => Value::Int(
            i64::try_from(count)
                .map_err(|_| ExecError::Other("aggregate count exceeds BIGINT".into()))?,
        ),
        AggregateKind::Sum => sum.map(Value::Float).unwrap_or(Value::Null),
        AggregateKind::Avg => match (sum, count) {
            (Some(sum), count) if count > 0 => Value::Float(sum / count as f64),
            _ => Value::Null,
        },
        AggregateKind::Min => min.unwrap_or(Value::Null),
        AggregateKind::Max => max.unwrap_or(Value::Null),
    })
}
