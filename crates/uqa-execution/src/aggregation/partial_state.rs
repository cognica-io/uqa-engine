//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encoding and merging of compact, mergeable aggregate partial states.

use super::accumulator::Transition;
use super::transitions::{observe_extreme, GenericState, StatisticsState, SumState};
use super::AggregateStatePlan;
use super::{AggregateAccumulator, DecimalValue, NumericInputKind, SQLError, ScalarExpr, Value};
use std::cmp::Ordering;
use uqa_sql::expr::IntervalFields;

pub(super) fn partial_schema(
    relation: uqa_sql::ast::InternalRelationId,
    group_types: &[Option<uqa_sql::ColumnType>],
) -> crate::RowSchema {
    let mut types = group_types.to_vec();
    types.push(None);
    crate::RowSchema::with_internal_relation_types(relation, types)
}

pub(super) fn encode_partial_group(
    group_values: Vec<Value>,
    accumulators: Vec<AggregateAccumulator>,
) -> crate::PhysicalRow {
    let mut values = group_values;
    values.push(Value::List(
        accumulators.into_iter().map(encode_accumulator).collect(),
    ));
    crate::PhysicalRow::from_values(values)
}

pub(super) fn decode_partial_group(
    row: crate::PhysicalRow,
    aggregate_targets: &[ScalarExpr],
    accumulator_budget: usize,
    group_count: usize,
) -> Result<(Vec<Value>, Vec<AggregateAccumulator>), SQLError> {
    let mut values = row.into_physical_values();
    if values.len() != group_count + 1 {
        return Err(SQLError::Internal(format!(
            "partial aggregate row has {} values, expected {}",
            values.len(),
            group_count + 1
        )));
    }
    let state = values.pop();
    let states = match state {
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
    Ok((values, accumulators))
}

// The 22-field spill record has a stable field order. Materialize it only at the codec boundary.
#[derive(Default)]
struct PartialFields {
    values: GenericState,
    statistics: StatisticsState,
}

impl PartialFields {
    fn from_transition(transition: Transition) -> Self {
        let mut fields = Self::default();
        match transition {
            Transition::Generic(state) => fields.values = *state,
            Transition::Count(count) => fields.values.sum.count = count,
            Transition::Sum(sum) => fields.values.sum = sum,
            Transition::Min(value) => fields.values.min = value,
            Transition::Max(value) => fields.values.max = value,
            Transition::BoolAnd(value) => fields.values.bool_and = value,
            Transition::BoolOr(value) => fields.values.bool_or = value,
            Transition::Statistics(state) => fields.statistics = *state,
            _ => {}
        }
        fields
    }

    fn into_transition(self, plan: AggregateStatePlan) -> Transition {
        if self.values.sum.real {
            return Transition::Sum(self.values.sum);
        }
        match plan {
            AggregateStatePlan::Generic => Transition::Generic(Box::new(self.values)),
            AggregateStatePlan::Count => Transition::Count(self.values.sum.count),
            AggregateStatePlan::Sum | AggregateStatePlan::SumReal => {
                Transition::Sum(self.values.sum)
            }
            AggregateStatePlan::Min => Transition::Min(self.values.min),
            AggregateStatePlan::Max => Transition::Max(self.values.max),
            AggregateStatePlan::BoolAnd => Transition::BoolAnd(self.values.bool_and),
            AggregateStatePlan::BoolOr => Transition::BoolOr(self.values.bool_or),
            AggregateStatePlan::Statistics => Transition::Statistics(Box::new(self.statistics)),
            other => Transition::for_plan(other),
        }
    }
}

fn encode_accumulator(accumulator: AggregateAccumulator) -> Value {
    let PartialFields { values, statistics } =
        PartialFields::from_transition(accumulator.transition);
    let GenericState {
        sum,
        min,
        max,
        bool_and,
        bool_or,
    } = values;
    Value::List(vec![
        unsigned_bytes(sum.count),
        float_bytes(sum.sum),
        Value::Bytes(sum.integer_sum.to_be_bytes().to_vec()),
        sum.decimal_sum.map_or(Value::Null, Value::Decimal),
        Value::Int(numeric_kind_code(sum.numeric_inputs)),
        min.unwrap_or(Value::Null),
        max.unwrap_or(Value::Null),
        bool_and.map_or(Value::Null, Value::Bool),
        bool_or.map_or(Value::Null, Value::Bool),
        unsigned_bytes(statistics.count),
        float_bytes(statistics.mean),
        float_bytes(statistics.m2),
        Value::Bool(statistics.moments_valid),
        Value::Bool(statistics.has_float),
        Value::Bool(statistics.nonzero_deviation),
        statistics.origin.map_or(Value::Null, Value::Decimal),
        statistics.sum.map_or(Value::Null, Value::Decimal),
        statistics.sum_squares.map_or(Value::Null, Value::Decimal),
        Value::Bool(sum.real),
        sum.interval_sum
            .map_or(Value::Null, |sum| Value::Temporal(sum.value())),
        unsigned_bytes(sum.interval_positive_infinity),
        unsigned_bytes(sum.interval_negative_infinity),
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
    if fields.len() != 22 {
        return Err(SQLError::Internal(format!(
            "partial aggregate accumulator has {} fields, expected 22",
            fields.len()
        )));
    }
    let mut fields = fields.into_iter();
    let mut state = PartialFields::default();
    let sum = &mut state.values.sum;
    let statistics = &mut state.statistics;
    sum.count = decode_unsigned(next(&mut fields)?, "count")?;
    sum.sum = decode_float(next(&mut fields)?, "sum")?;
    sum.integer_sum = decode_i128(next(&mut fields)?, "integer sum")?;
    sum.decimal_sum = optional_decimal(next(&mut fields)?)?;
    sum.numeric_inputs = decode_numeric_kind(next(&mut fields)?)?;
    state.values.min = optional_value(next(&mut fields)?);
    state.values.max = optional_value(next(&mut fields)?);
    state.values.bool_and = optional_bool(next(&mut fields)?, "bool_and")?;
    state.values.bool_or = optional_bool(next(&mut fields)?, "bool_or")?;
    statistics.count = decode_unsigned(next(&mut fields)?, "statistics count")?;
    statistics.mean = decode_float(next(&mut fields)?, "statistics mean")?;
    statistics.m2 = decode_float(next(&mut fields)?, "statistics m2")?;
    statistics.moments_valid = match next(&mut fields)? {
        Value::Bool(value) => value,
        _ => return Err(SQLError::Internal("invalid statistics moments flag".into())),
    };
    statistics.has_float = match next(&mut fields)? {
        Value::Bool(value) => value,
        _ => return Err(SQLError::Internal("invalid statistics float flag".into())),
    };
    statistics.nonzero_deviation = match next(&mut fields)? {
        Value::Bool(value) => value,
        _ => {
            return Err(SQLError::Internal(
                "invalid statistics nonzero-deviation flag".into(),
            ))
        }
    };
    statistics.origin = optional_decimal(next(&mut fields)?)?;
    statistics.sum = optional_decimal(next(&mut fields)?)?;
    statistics.sum_squares = optional_decimal(next(&mut fields)?)?;
    sum.real = match next(&mut fields)? {
        Value::Bool(real) => real,
        _ => {
            return Err(SQLError::Internal(
                "invalid aggregate float width flag".into(),
            ))
        }
    };
    sum.interval_sum = match next(&mut fields)? {
        Value::Null => None,
        Value::Temporal(sum) => Some(IntervalFields::of(&sum).ok_or_else(|| {
            SQLError::Internal("partial aggregate interval sum is not an interval".into())
        })?),
        _ => {
            return Err(SQLError::Internal(
                "invalid partial aggregate interval sum".into(),
            ))
        }
    };
    sum.interval_positive_infinity =
        decode_unsigned(next(&mut fields)?, "positive interval infinity count")?;
    sum.interval_negative_infinity =
        decode_unsigned(next(&mut fields)?, "negative interval infinity count")?;
    Ok(AggregateAccumulator::with_budget(
        state.into_transition(AggregateStatePlan::builtin(name)),
        budget_bytes,
    ))
}

fn next(values: &mut impl Iterator<Item = Value>) -> Result<Value, SQLError> {
    values
        .next()
        .ok_or_else(|| SQLError::Internal("partial aggregate field is missing".into()))
}

pub(super) fn merge_accumulators(
    target: &mut AggregateAccumulator,
    source: AggregateAccumulator,
    enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
) -> Result<(), SQLError> {
    let comparison = target.enum_comparison.as_deref();
    let merge_extreme = |left: &mut Option<Value>, right: Option<Value>, order| {
        if let Some(value) = right {
            observe_extreme(left, &value, order, enums, comparison)?;
        }
        Ok::<_, SQLError>(())
    };
    match (&mut target.transition, source.transition) {
        (Transition::Count(left), Transition::Count(right)) => {
            *left = left
                .checked_add(right)
                .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
        }
        (Transition::Sum(left), Transition::Sum(right)) => merge_sums(left, &right)?,
        (Transition::Min(left), Transition::Min(right)) => {
            merge_extreme(left, right, Ordering::Greater)?;
        }
        (Transition::Max(left), Transition::Max(right)) => {
            merge_extreme(left, right, Ordering::Less)?;
        }
        (Transition::BoolAnd(left), Transition::BoolAnd(right)) => merge_bool(left, right, true),
        (Transition::BoolOr(left), Transition::BoolOr(right)) => merge_bool(left, right, false),
        (Transition::Statistics(left), Transition::Statistics(right)) => {
            merge_statistics(left, *right)?;
        }
        (Transition::Generic(left), Transition::Generic(right)) => {
            merge_sums(&mut left.sum, &right.sum)?;
            merge_extreme(&mut left.min, right.min, Ordering::Greater)?;
            merge_extreme(&mut left.max, right.max, Ordering::Less)?;
            merge_bool(&mut left.bool_and, right.bool_and, true);
            merge_bool(&mut left.bool_or, right.bool_or, false);
        }
        _ => {
            return Err(SQLError::Internal(
                "partial aggregate transitions do not match their target".into(),
            ))
        }
    }
    Ok(())
}

fn merge_bool(left: &mut Option<bool>, right: Option<bool>, conjunction: bool) {
    if let Some(value) = right {
        *left = Some(if conjunction {
            left.unwrap_or(true) && value
        } else {
            left.unwrap_or(false) || value
        });
    }
}

fn merge_sums(target: &mut SumState, source: &SumState) -> Result<(), SQLError> {
    let target_float_total = numeric_total_as_float(target)?;
    let source_float_total = numeric_total_as_float(source)?;
    let target_decimal = decimal_component(target)?;
    let source_decimal = decimal_component(source)?;
    let has_decimal = target.numeric_inputs.has_decimal() || source.numeric_inputs.has_decimal();
    let has_float =
        numeric_has_float(target.numeric_inputs) || numeric_has_float(source.numeric_inputs);
    let target_empty = target.count == 0;

    target.count = target
        .count
        .checked_add(source.count)
        .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
    target.integer_sum = target
        .integer_sum
        .checked_add(source.integer_sum)
        .ok_or_else(|| SQLError::TypeMismatch("integer aggregate overflow".into()))?;
    // `interval_avg_combine` adds the other state's sum only when it saw a finite input.
    target.interval_sum = match (target.interval_sum, source.interval_sum) {
        (Some(left), Some(right)) => Some(left.plus(right)?),
        (Some(sum), None) | (None, Some(sum)) => Some(sum),
        (None, None) => None,
    };
    target.interval_positive_infinity = target
        .interval_positive_infinity
        .checked_add(source.interval_positive_infinity)
        .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
    target.interval_negative_infinity = target
        .interval_negative_infinity
        .checked_add(source.interval_negative_infinity)
        .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
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
        if target_empty {
            source_float_total
        } else if source.count == 0 {
            target_float_total
        } else {
            target.combine_float_sum(target_float_total, source_float_total)?
        }
    } else {
        0.0
    };
    Ok(())
}

fn merge_statistics(target: &mut StatisticsState, source: StatisticsState) -> Result<(), SQLError> {
    if source.count == 0 {
        return Ok(());
    }
    if target.count == 0 {
        *target = source;
        return Ok(());
    }
    let combined = target
        .count
        .checked_add(source.count)
        .ok_or_else(|| SQLError::TypeMismatch("statistical aggregate count overflow".into()))?;
    let moments_valid = target.moments_valid && source.moments_valid;
    let has_float = target.has_float || source.has_float;
    if has_float && !moments_valid {
        return Err(SQLError::TypeMismatch(
            "numeric statistical state does not fit double precision".into(),
        ));
    }
    if moments_valid {
        let delta = source.mean - target.mean;
        let left = target.count as f64;
        let right = source.count as f64;
        let total = combined as f64;
        target.mean += delta * right / total;
        target.m2 += source.m2 + delta * delta * left * right / total;
    }
    target.moments_valid = moments_valid;
    target.has_float = has_float;
    if has_float {
        target.origin = None;
        target.sum = None;
        target.sum_squares = None;
    } else {
        merge_exact_statistics(target, &source)?;
    }
    target.count = combined;
    Ok(())
}

fn merge_exact_statistics(
    target: &mut StatisticsState,
    source: &StatisticsState,
) -> Result<(), SQLError> {
    let target_origin = target
        .origin
        .as_ref()
        .ok_or_else(|| SQLError::Internal("numeric statistical state omitted its origin".into()))?;
    let source_origin = source.origin.as_ref().ok_or_else(|| {
        SQLError::Internal("numeric statistical partial state omitted its origin".into())
    })?;
    let target_sum = target.sum.as_ref().ok_or_else(|| {
        SQLError::Internal("numeric statistical state omitted its deviation sum".into())
    })?;
    let source_sum = source.sum.as_ref().ok_or_else(|| {
        SQLError::Internal("numeric statistical partial state omitted its deviation sum".into())
    })?;
    let target_squares = target.sum_squares.as_ref().ok_or_else(|| {
        SQLError::Internal("numeric statistical state omitted its deviation square sum".into())
    })?;
    let source_squares = source.sum_squares.as_ref().ok_or_else(|| {
        SQLError::Internal(
            "numeric statistical partial state omitted its deviation square sum".into(),
        )
    })?;
    target.nonzero_deviation |= source.nonzero_deviation || source_origin != target_origin;
    if target_squares.is_nan() || source_squares.is_nan() {
        let nan = target_squares.checked_add(source_squares).ok_or_else(|| {
            SQLError::Internal("numeric special partial states did not produce NaN".into())
        })?;
        target.sum = Some(nan.clone());
        target.sum_squares = Some(nan);
        return Ok(());
    }
    let source_count = DecimalValue::from_i128(i128::from(source.count))
        .ok_or_else(|| SQLError::TypeMismatch("statistical aggregate count overflow".into()))?;
    let shift = source_origin.checked_sub(target_origin).ok_or_else(|| {
        SQLError::TypeMismatch("numeric statistical origin shift overflow".into())
    })?;
    let shifted_sum = shift
        .checked_mul(&source_count)
        .and_then(|value| value.checked_add(source_sum))
        .ok_or_else(|| {
            SQLError::TypeMismatch("numeric statistical deviation sum overflow".into())
        })?;
    let twice_source_sum = source_sum
        .checked_mul(&DecimalValue::from_i64(2))
        .ok_or_else(|| {
            SQLError::TypeMismatch("numeric statistical deviation sum overflow".into())
        })?;
    let shifted_squares = shift
        .checked_mul(&shift)
        .and_then(|value| value.checked_mul(&source_count))
        .and_then(|shift_square| {
            shift
                .checked_mul(&twice_source_sum)
                .and_then(|cross| shift_square.checked_add(&cross))
        })
        .and_then(|adjustment| adjustment.checked_add(source_squares))
        .ok_or_else(|| {
            SQLError::TypeMismatch("numeric statistical deviation square sum overflow".into())
        })?;
    target.sum = target_sum.checked_add(&shifted_sum);
    target.sum_squares = target_squares.checked_add(&shifted_squares);
    if target.sum.is_none() || target.sum_squares.is_none() {
        return Err(SQLError::TypeMismatch(
            "numeric statistical partial-state merge overflow".into(),
        ));
    }
    Ok(())
}

fn numeric_total_as_float(accumulator: &SumState) -> Result<f64, SQLError> {
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

fn decimal_component(accumulator: &SumState) -> Result<Option<DecimalValue>, SQLError> {
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
