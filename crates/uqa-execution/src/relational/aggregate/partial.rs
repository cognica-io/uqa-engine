//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compact spill representation for mergeable built-in aggregate states.

use super::{fold, AggregateSpec, ExecError, ExecResult, RowSchema, Value};
use crate::PhysicalRow;
use fold::{AggFold, GroupState};

pub(super) fn schema(relation: uqa_sql::ast::InternalRelationId, group_count: usize) -> RowSchema {
    RowSchema::with_internal_relation_types(relation, vec![None; group_count + 1])
}

pub(super) fn encode_group(state: GroupState) -> PhysicalRow {
    let mut values = state.key_values;
    values.push(Value::List(
        state.folds.into_iter().map(encode_fold).collect(),
    ));
    PhysicalRow::from_values(values)
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
    row: PhysicalRow,
    group_count: usize,
    aggregates: &[AggregateSpec],
) -> ExecResult<GroupState> {
    let mut values = row.into_physical_values();
    if values.len() != group_count + 1 {
        return Err(ExecError::Other(format!(
            "partial aggregate row has {} values, expected {}",
            values.len(),
            group_count + 1
        )));
    }
    let state = values.pop();
    let states = match state {
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
    Ok(GroupState {
        folds,
        key_values: values,
    })
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
