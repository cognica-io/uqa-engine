//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compact spill representation for mergeable built-in aggregate states.

use super::{fold, AggregateSpec, ExecError, ExecResult, ResultRow, RowSchema, Value};
use fold::{AggFold, GroupState};

const PARTIAL_GROUP_PREFIX: &str = "__uqa_builtin_group_";
const PARTIAL_STATE_COLUMN: &str = "__uqa_builtin_state";

pub(super) fn group_column(index: usize) -> String {
    format!("{PARTIAL_GROUP_PREFIX}{index}")
}

pub(super) fn schema(group_count: usize) -> RowSchema {
    let mut columns = (0..group_count).map(group_column).collect::<Vec<_>>();
    columns.push(PARTIAL_STATE_COLUMN.into());
    RowSchema::new(columns)
}

pub(super) fn encode_group(state: GroupState) -> ResultRow {
    let mut row = ResultRow::new();
    for (index, value) in state.key_values.into_iter().enumerate() {
        row.insert(group_column(index), value);
    }
    row.insert(
        PARTIAL_STATE_COLUMN.into(),
        Value::List(state.folds.into_iter().map(encode_fold).collect()),
    );
    row
}

fn encode_fold(fold: AggFold) -> Value {
    Value::List(vec![
        Value::Bytes(fold.count.to_be_bytes().to_vec()),
        fold.sum.map_or(Value::Null, |value| {
            Value::Bytes(value.to_bits().to_be_bytes().to_vec())
        }),
        fold.min.unwrap_or(Value::Null),
        fold.max.unwrap_or(Value::Null),
    ])
}

pub(super) fn decode_group(
    mut row: ResultRow,
    group_count: usize,
    aggregates: &[AggregateSpec],
) -> ExecResult<GroupState> {
    let key_values = (0..group_count)
        .map(|index| {
            row.remove(&group_column(index)).ok_or_else(|| {
                ExecError::Other(format!("partial aggregate group key {index} is missing"))
            })
        })
        .collect::<ExecResult<Vec<_>>>()?;
    let states = match row.remove(PARTIAL_STATE_COLUMN) {
        Some(Value::List(states)) => states,
        _ => {
            return Err(ExecError::Other(
                "partial aggregate state vector is missing".into(),
            ))
        }
    };
    if states.len() != aggregates.len() {
        return Err(ExecError::Other(format!(
            "partial aggregate state count {} does not match aggregate count {}",
            states.len(),
            aggregates.len()
        )));
    }
    let folds = states
        .into_iter()
        .zip(aggregates)
        .map(|(state, aggregate)| decode_fold(state, aggregate))
        .collect::<ExecResult<Vec<_>>>()?;
    Ok(GroupState { folds, key_values })
}

fn decode_fold(state: Value, aggregate: &AggregateSpec) -> ExecResult<AggFold> {
    let Value::List(fields) = state else {
        return Err(ExecError::Other(
            "partial aggregate fold is not a list".into(),
        ));
    };
    let [count, sum, min, max]: [Value; 4] = fields.try_into().map_err(|fields: Vec<Value>| {
        ExecError::Other(format!(
            "partial aggregate fold has {} fields, expected 4",
            fields.len()
        ))
    })?;
    let mut fold = AggFold::new(1, aggregate.distinct);
    fold.count = decode_u64(count, "count")?;
    fold.sum = match sum {
        Value::Null => None,
        value => Some(f64::from_bits(decode_u64(value, "sum")?)),
    };
    fold.min = match min {
        Value::Null => None,
        value => Some(value),
    };
    fold.max = match max {
        Value::Null => None,
        value => Some(value),
    };
    Ok(fold)
}

fn decode_u64(value: Value, description: &str) -> ExecResult<u64> {
    let Value::Bytes(bytes) = value else {
        return Err(ExecError::Other(format!(
            "partial aggregate {description} is not bytes"
        )));
    };
    let bytes: [u8; 8] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        ExecError::Other(format!(
            "partial aggregate {description} has {} bytes, expected 8",
            bytes.len()
        ))
    })?;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn merge_group(target: &mut GroupState, source: GroupState) -> ExecResult<()> {
    if target.key_values != source.key_values || target.folds.len() != source.folds.len() {
        return Err(ExecError::Other(
            "incompatible partial aggregate groups cannot be merged".into(),
        ));
    }
    for (target, source) in target.folds.iter_mut().zip(source.folds) {
        fold::merge_fold(target, source)?;
    }
    Ok(())
}
