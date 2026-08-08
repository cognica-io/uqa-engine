//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encoding and merging of compact, mergeable aggregate partial states.

use super::{
    value_gt, value_lt, AggregateAccumulator, DecimalValue, NumericInputKind, ResultRow, SQLError,
    ScalarExpr, Value,
};

pub(super) const PARTIAL_STATE_COLUMN: &str = "__uqa_partial_aggregate_state";
const PARTIAL_GROUP_PREFIX: &str = "__uqa_partial_group_";

pub(super) fn partial_group_column(index: usize) -> String {
    format!("{PARTIAL_GROUP_PREFIX}{index}")
}

pub(super) fn partial_schema(group_count: usize) -> uqa_execution::RowSchema {
    let mut columns = (0..group_count)
        .map(partial_group_column)
        .collect::<Vec<_>>();
    columns.push(PARTIAL_STATE_COLUMN.to_string());
    uqa_execution::RowSchema::new(columns)
}

pub(super) fn encode_partial_group(
    group_values: Vec<Value>,
    accumulators: Vec<AggregateAccumulator>,
) -> ResultRow {
    let mut row = ResultRow::new();
    for (index, value) in group_values.into_iter().enumerate() {
        row.insert(partial_group_column(index), value);
    }
    row.insert(
        PARTIAL_STATE_COLUMN.to_string(),
        Value::List(accumulators.into_iter().map(encode_accumulator).collect()),
    );
    row
}

pub(super) fn decode_partial_group(
    mut row: ResultRow,
    aggregate_targets: &[ScalarExpr],
    accumulator_budget: usize,
    group_count: usize,
) -> Result<(Vec<Value>, Vec<AggregateAccumulator>), SQLError> {
    let group_values = (0..group_count)
        .map(|index| {
            row.remove(&partial_group_column(index)).ok_or_else(|| {
                SQLError::Internal(format!("partial aggregate group key {index} is missing"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let states = match row.remove(PARTIAL_STATE_COLUMN) {
        Some(Value::List(states)) => states,
        _ => {
            return Err(SQLError::Internal(
                "partial aggregate state vector is missing".into(),
            ))
        }
    };
    if states.len() != aggregate_targets.len() {
        return Err(SQLError::Internal(format!(
            "partial aggregate state count {} does not match target count {}",
            states.len(),
            aggregate_targets.len()
        )));
    }
    let accumulators = states
        .into_iter()
        .zip(aggregate_targets)
        .map(|(state, target)| decode_accumulator(state, target, accumulator_budget))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((group_values, accumulators))
}

fn encode_accumulator(accumulator: AggregateAccumulator) -> Value {
    Value::List(vec![
        unsigned_bytes(accumulator.count),
        float_bytes(accumulator.sum),
        Value::Bytes(accumulator.integer_sum.to_be_bytes().to_vec()),
        accumulator.decimal_sum.map_or(Value::Null, Value::Decimal),
        Value::Int(numeric_kind_code(accumulator.numeric_inputs)),
        accumulator.min.unwrap_or(Value::Null),
        accumulator.max.unwrap_or(Value::Null),
        accumulator.bool_and.map_or(Value::Null, Value::Bool),
        accumulator.bool_or.map_or(Value::Null, Value::Bool),
        unsigned_bytes(accumulator.statistics_count),
        float_bytes(accumulator.statistics_mean),
        float_bytes(accumulator.statistics_m2),
        Value::Bool(accumulator.statistics_has_float),
    ])
}

fn decode_accumulator(
    state: Value,
    target: &ScalarExpr,
    budget_bytes: usize,
) -> Result<AggregateAccumulator, SQLError> {
    let ScalarExpr::Func { name, .. } = target else {
        return Err(SQLError::Internal(
            "partial aggregate target is not a function".into(),
        ));
    };
    let Value::List(fields) = state else {
        return Err(SQLError::Internal(
            "partial aggregate accumulator is not a list".into(),
        ));
    };
    if fields.len() != 13 {
        return Err(SQLError::Internal(format!(
            "partial aggregate accumulator has {} fields, expected 13",
            fields.len()
        )));
    }
    let mut fields = fields.into_iter();
    let mut accumulator = AggregateAccumulator::builtin_with_budget(name, budget_bytes);
    accumulator.count = decode_unsigned(next(&mut fields)?, "count")?;
    accumulator.sum = decode_float(next(&mut fields)?, "sum")?;
    accumulator.integer_sum = decode_i128(next(&mut fields)?, "integer sum")?;
    accumulator.decimal_sum = optional_decimal(next(&mut fields)?)?;
    accumulator.numeric_inputs = decode_numeric_kind(next(&mut fields)?)?;
    accumulator.min = optional_value(next(&mut fields)?);
    accumulator.max = optional_value(next(&mut fields)?);
    accumulator.bool_and = optional_bool(next(&mut fields)?, "bool_and")?;
    accumulator.bool_or = optional_bool(next(&mut fields)?, "bool_or")?;
    accumulator.statistics_count = decode_unsigned(next(&mut fields)?, "statistics count")?;
    accumulator.statistics_mean = decode_float(next(&mut fields)?, "statistics mean")?;
    accumulator.statistics_m2 = decode_float(next(&mut fields)?, "statistics m2")?;
    accumulator.statistics_has_float = match next(&mut fields)? {
        Value::Bool(value) => value,
        _ => return Err(SQLError::Internal("invalid statistics float flag".into())),
    };
    Ok(accumulator)
}

fn next(values: &mut impl Iterator<Item = Value>) -> Result<Value, SQLError> {
    values
        .next()
        .ok_or_else(|| SQLError::Internal("partial aggregate field is missing".into()))
}

pub(super) fn merge_accumulators(
    target: &mut AggregateAccumulator,
    source: AggregateAccumulator,
) -> Result<(), SQLError> {
    let target_float_total = numeric_total_as_float(target)?;
    let source_float_total = numeric_total_as_float(&source)?;
    let target_decimal = decimal_component(target)?;
    let source_decimal = decimal_component(&source)?;
    let has_decimal = target.numeric_inputs.has_decimal() || source.numeric_inputs.has_decimal();
    let has_float =
        numeric_has_float(target.numeric_inputs) || numeric_has_float(source.numeric_inputs);

    target.count = target
        .count
        .checked_add(source.count)
        .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
    target.integer_sum = target
        .integer_sum
        .checked_add(source.integer_sum)
        .ok_or_else(|| SQLError::TypeMismatch("integer aggregate overflow".into()))?;
    target.numeric_inputs = numeric_kind(has_decimal, has_float);
    target.decimal_sum = if has_decimal {
        Some(match (target_decimal, source_decimal) {
            (Some(left), Some(right)) => left
                .checked_add(&right)
                .ok_or_else(|| SQLError::TypeMismatch("decimal aggregate overflow".into()))?,
            (Some(value), None) | (None, Some(value)) => value,
            (None, None) => DecimalValue::from_i64(0),
        })
    } else {
        None
    };
    target.sum = if has_float {
        target_float_total + source_float_total
    } else {
        0.0
    };
    merge_statistics(target, &source)?;
    merge_min(&mut target.min, source.min);
    merge_max(&mut target.max, source.max);
    target.bool_and = match (target.bool_and, source.bool_and) {
        (Some(left), Some(right)) => Some(left && right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    target.bool_or = match (target.bool_or, source.bool_or) {
        (Some(left), Some(right)) => Some(left || right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    Ok(())
}

fn merge_statistics(
    target: &mut AggregateAccumulator,
    source: &AggregateAccumulator,
) -> Result<(), SQLError> {
    if source.statistics_count == 0 {
        return Ok(());
    }
    if target.statistics_count == 0 {
        target.statistics_count = source.statistics_count;
        target.statistics_mean = source.statistics_mean;
        target.statistics_m2 = source.statistics_m2;
        target.statistics_has_float = source.statistics_has_float;
        return Ok(());
    }
    let combined = target
        .statistics_count
        .checked_add(source.statistics_count)
        .ok_or_else(|| SQLError::TypeMismatch("statistical aggregate count overflow".into()))?;
    let delta = source.statistics_mean - target.statistics_mean;
    let left = target.statistics_count as f64;
    let right = source.statistics_count as f64;
    let total = combined as f64;
    target.statistics_mean += delta * right / total;
    target.statistics_m2 += source.statistics_m2 + delta * delta * left * right / total;
    target.statistics_count = combined;
    target.statistics_has_float |= source.statistics_has_float;
    Ok(())
}

fn numeric_total_as_float(accumulator: &AggregateAccumulator) -> Result<f64, SQLError> {
    if numeric_has_float(accumulator.numeric_inputs) {
        return Ok(accumulator.sum);
    }
    if accumulator.numeric_inputs.has_decimal() {
        return accumulator
            .decimal_sum
            .as_ref()
            .and_then(DecimalValue::to_f64)
            .ok_or_else(|| SQLError::TypeMismatch("decimal aggregate does not fit float".into()));
    }
    Ok(accumulator.integer_sum as f64)
}

fn decimal_component(accumulator: &AggregateAccumulator) -> Result<Option<DecimalValue>, SQLError> {
    if accumulator.numeric_inputs.has_decimal() {
        return Ok(accumulator.decimal_sum.clone());
    }
    if accumulator.integer_sum == 0 {
        return Ok(None);
    }
    DecimalValue::from_i128(accumulator.integer_sum)
        .map(Some)
        .ok_or_else(|| SQLError::TypeMismatch("integer aggregate does not fit decimal".into()))
}

fn merge_min(target: &mut Option<Value>, source: Option<Value>) {
    if let Some(source) = source {
        if target
            .as_ref()
            .is_none_or(|current| value_lt(&source, current))
        {
            *target = Some(source);
        }
    }
}

fn merge_max(target: &mut Option<Value>, source: Option<Value>) {
    if let Some(source) = source {
        if target
            .as_ref()
            .is_none_or(|current| value_gt(&source, current))
        {
            *target = Some(source);
        }
    }
}

fn numeric_kind_code(kind: NumericInputKind) -> i64 {
    match kind {
        NumericInputKind::Integers => 0,
        NumericInputKind::Decimals => 1,
        NumericInputKind::Floats => 2,
        NumericInputKind::DecimalsAndFloats => 3,
    }
}

fn decode_numeric_kind(value: Value) -> Result<NumericInputKind, SQLError> {
    match value {
        Value::Int(0) => Ok(NumericInputKind::Integers),
        Value::Int(1) => Ok(NumericInputKind::Decimals),
        Value::Int(2) => Ok(NumericInputKind::Floats),
        Value::Int(3) => Ok(NumericInputKind::DecimalsAndFloats),
        _ => Err(SQLError::Internal("invalid partial numeric kind".into())),
    }
}

fn numeric_has_float(kind: NumericInputKind) -> bool {
    kind.has_float()
}

fn numeric_kind(has_decimal: bool, has_float: bool) -> NumericInputKind {
    match (has_decimal, has_float) {
        (false, false) => NumericInputKind::Integers,
        (true, false) => NumericInputKind::Decimals,
        (false, true) => NumericInputKind::Floats,
        (true, true) => NumericInputKind::DecimalsAndFloats,
    }
}

fn unsigned_bytes(value: u64) -> Value {
    Value::Bytes(value.to_be_bytes().to_vec())
}

fn float_bytes(value: f64) -> Value {
    unsigned_bytes(value.to_bits())
}

fn decode_unsigned(value: Value, name: &str) -> Result<u64, SQLError> {
    let Value::Bytes(bytes) = value else {
        return Err(SQLError::Internal(format!("invalid partial {name}")));
    };
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| SQLError::Internal(format!("invalid partial {name} width")))?;
    Ok(u64::from_be_bytes(bytes))
}

fn decode_float(value: Value, name: &str) -> Result<f64, SQLError> {
    decode_unsigned(value, name).map(f64::from_bits)
}

fn decode_i128(value: Value, name: &str) -> Result<i128, SQLError> {
    let Value::Bytes(bytes) = value else {
        return Err(SQLError::Internal(format!("invalid partial {name}")));
    };
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| SQLError::Internal(format!("invalid partial {name} width")))?;
    Ok(i128::from_be_bytes(bytes))
}

fn optional_decimal(value: Value) -> Result<Option<DecimalValue>, SQLError> {
    match value {
        Value::Null => Ok(None),
        Value::Decimal(value) => Ok(Some(value)),
        _ => Err(SQLError::Internal("invalid partial decimal sum".into())),
    }
}

fn optional_bool(value: Value, name: &str) -> Result<Option<bool>, SQLError> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(value) => Ok(Some(value)),
        _ => Err(SQLError::Internal(format!("invalid partial {name}"))),
    }
}

fn optional_value(value: Value) -> Option<Value> {
    (!matches!(value, Value::Null)).then_some(value)
}
