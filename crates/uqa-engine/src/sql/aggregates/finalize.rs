//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in aggregate finalization and percentile/statistical helpers.

use super::{
    cast_value, core_value_to_json, distinct_key, value_as_f64, value_to_json_text,
    AggregateAccumulator, AggregateValueBuffer, DecimalValue, ProjectionPlan, SQLError, ScalarExpr,
    Value,
};
use uqa_core::ArrayValue;

pub(in crate::sql) fn aggregate_value(
    name: &str,
    acc: &AggregateAccumulator,
) -> Result<Value, SQLError> {
    aggregate_value_with_args(name, acc, &[])
}

pub(in crate::sql) fn aggregate_value_with_args(
    name: &str,
    acc: &AggregateAccumulator,
    args: &[ScalarExpr],
) -> Result<Value, SQLError> {
    if let Some(value) = acc.registered_value() {
        return value;
    }
    let lname = name.to_ascii_lowercase();

    let value = match lname.as_str() {
        "count" => Value::Int(
            i64::try_from(acc.count)
                .map_err(|_| SQLError::TypeMismatch("aggregate count exceeds BIGINT".into()))?,
        ),
        "sum" => {
            if acc.count == 0 {
                Value::Null
            } else if acc.numeric_inputs.decimal_without_float() {
                acc.decimal_sum.clone().map_or(Value::Null, Value::Decimal)
            } else if acc.numeric_inputs.all_integers() {
                Value::Int(i64::try_from(acc.integer_sum).map_err(|_| {
                    SQLError::TypeMismatch("integer aggregate result exceeds BIGINT".into())
                })?)
            } else {
                Value::Float(acc.sum)
            }
        }
        "avg" => {
            if acc.count == 0 {
                Value::Null
            } else if acc.numeric_inputs.decimal_without_float() {
                let divisor = DecimalValue::from_i64(i64::try_from(acc.count).map_err(|_| {
                    SQLError::TypeMismatch("aggregate count exceeds BIGINT".into())
                })?);
                let average = acc
                    .decimal_sum
                    .as_ref()
                    .and_then(|sum| sum.checked_div_postgres(&divisor))
                    .ok_or_else(|| SQLError::TypeMismatch("decimal AVG overflow".into()))?;
                Value::Decimal(average)
            } else if acc.numeric_inputs.all_integers() {
                let sum = DecimalValue::from_i128(acc.integer_sum)
                    .ok_or_else(|| SQLError::TypeMismatch("integer AVG overflow".into()))?;
                let divisor = DecimalValue::from_i128(i128::from(acc.count)).ok_or_else(|| {
                    SQLError::TypeMismatch("aggregate count exceeds NUMERIC".into())
                })?;
                let average = sum
                    .checked_div_postgres(&divisor)
                    .ok_or_else(|| SQLError::TypeMismatch("integer AVG overflow".into()))?;
                Value::Decimal(average)
            } else {
                Value::Float(acc.sum / acc.count as f64)
            }
        }
        "min" => acc.min.clone().unwrap_or(Value::Null),
        "max" => acc.max.clone().unwrap_or(Value::Null),
        "string_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            // Separator: literal second positional arg, or empty.
            let sep = match args.get(1) {
                Some(ScalarExpr::Literal(Value::Str(s))) => s.clone(),
                _ => String::new(),
            };
            let parts: Vec<String> = ordered_values
                .iter()
                .map(|v| match v {
                    Value::Null => Ok(None),
                    Value::Str(s) => Ok(Some(s.clone())),
                    Value::FixedChar(s) => Ok(Some(s.trim_end_matches(' ').to_string())),
                    Value::Int(n) => Ok(Some(n.to_string())),
                    Value::Float(f) => Ok(Some(f.to_string())),
                    Value::Decimal(d) => Ok(Some(d.to_sql_string())),
                    Value::Bool(b) => Ok(Some(b.to_string())),
                    Value::Temporal(t) => Ok(Some(t.to_sql_string())),
                    Value::Bytes(_)
                    | Value::Json(_)
                    | Value::JsonB(_)
                    | Value::Array(_)
                    | Value::List(_)
                    | Value::Row(_)
                    | Value::Record(_)
                    | Value::Map(_) => Err(SQLError::TypeMismatch(format!(
                        "string_agg requires a text-coercible value, got {v:?}"
                    ))),
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
            Value::Str(parts.join(&sep))
        }
        "array_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            ArrayValue::try_new(ordered_values)
                .map(Value::Array)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "cannot accumulate arrays of different dimensionality".into(),
                    )
                })?
        }
        "json_agg" | "jsonb_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            let text = format!(
                "[{}]",
                ordered_values
                    .iter()
                    .map(value_to_json_text)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if lname == "jsonb_agg" {
                cast_value(&Value::Str(text), "jsonb")?
            } else {
                Value::Json(text)
            }
        }
        "json_object_agg" | "jsonb_object_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            let mut fields = Vec::with_capacity(ordered_values.len());
            for value in ordered_values {
                let Value::List(pair) = value else {
                    return Err(SQLError::Internal(
                        "JSON object aggregate retained a non-pair value".into(),
                    ));
                };
                if pair.len() != 2 {
                    return Err(SQLError::Internal(
                        "JSON object aggregate retained a malformed pair".into(),
                    ));
                }
                if matches!(pair[0], Value::Null) {
                    return Err(SQLError::TypeMismatch(
                        "JSON object aggregate key must not be NULL".into(),
                    ));
                }
                let key = serde_json::Value::String(aggregate_json_key(&pair[0])).to_string();
                fields.push((key, value_to_json_text(&pair[1])));
            }
            if fields.is_empty() {
                Value::Null
            } else if lname == "jsonb_object_agg" {
                let text = fields
                    .into_iter()
                    .map(|(key, value)| format!("{key}:{value}"))
                    .collect::<Vec<_>>()
                    .join(",");
                cast_value(&Value::Str(format!("{{{text}}}")), "jsonb")?
            } else {
                let text = fields
                    .into_iter()
                    .map(|(key, value)| format!("{key} : {value}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                Value::Json(format!("{{ {text} }}"))
            }
        }
        "bool_and" => match acc.bool_and {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        },
        "bool_or" => match acc.bool_or {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        },
        "stddev" | "stddev_samp" => {
            if acc.statistics_count < 2 {
                return Ok(Value::Null);
            }
            statistical_standard_deviation(acc, true)?
        }
        "stddev_pop" => {
            if acc.statistics_count == 0 {
                return Ok(Value::Null);
            }
            statistical_standard_deviation(acc, false)?
        }
        "variance" | "var_samp" => {
            if acc.statistics_count < 2 {
                return Ok(Value::Null);
            }
            statistical_variance(acc, true)?
        }
        "var_pop" => {
            if acc.statistics_count == 0 {
                return Ok(Value::Null);
            }
            statistical_variance(acc, false)?
        }
        "percentile_cont" => {
            let frac = percentile_fraction(args)?;
            percentile_cont(&acc.values, frac)?.map_or(Value::Null, Value::Float)
        }
        "percentile_disc" => {
            let frac = percentile_fraction(args)?;
            percentile_disc(&acc.values, frac)?.unwrap_or(Value::Null)
        }
        "mode" => mode_value(&acc.values)?,
        _ => return Err(SQLError::UnknownFunction(format!("aggregate `{name}`"))),
    };
    Ok(value)
}

pub(in crate::sql) fn percentile_fraction(args: &[ScalarExpr]) -> Result<f64, SQLError> {
    let fraction = match args.first() {
        Some(ScalarExpr::Literal(Value::Float(f))) => *f,
        Some(ScalarExpr::Literal(Value::Int(n))) => *n as f64,
        Some(ScalarExpr::Literal(Value::Decimal(d))) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch("percentile fraction is outside floating-point range".into())
        })?,
        Some(value) => {
            return Err(SQLError::TypeMismatch(format!(
                "percentile fraction must be a numeric literal, got {value:?}"
            )))
        }
        None => {
            return Err(SQLError::BadArity {
                name: "percentile".into(),
                expected: "fraction argument".into(),
                actual: 0,
            })
        }
    };
    if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return Err(SQLError::TypeMismatch(format!(
            "percentile fraction must be between 0 and 1, got {fraction}"
        )));
    }
    Ok(fraction)
}

pub(in crate::sql) fn aggregate_json_key(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) => s.clone(),
        Value::FixedChar(s) => s.trim_end_matches(' ').to_string(),
        Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::Json(text) | Value::JsonB(text) => text.clone(),
        Value::Array(_) => uqa_sql::expr::value_to_string(value),
        Value::List(_) | Value::Row(_) | Value::Record(_) | Value::Map(_) => {
            serde_json::to_string(&core_value_to_json(value))
                .unwrap_or_else(|_| format!("{value:?}"))
        }
    }
}

fn statistical_variance(
    accumulator: &AggregateAccumulator,
    sample: bool,
) -> Result<Value, SQLError> {
    if accumulator.statistics_has_float {
        let divisor = if sample {
            accumulator.statistics_count as f64 - 1.0
        } else {
            accumulator.statistics_count as f64
        };
        return Ok(Value::Float(accumulator.statistics_m2 / divisor));
    }
    numeric_statistical_variance(accumulator, sample).map(Value::Decimal)
}

fn statistical_standard_deviation(
    accumulator: &AggregateAccumulator,
    sample: bool,
) -> Result<Value, SQLError> {
    if accumulator.statistics_has_float {
        let divisor = if sample {
            accumulator.statistics_count as f64 - 1.0
        } else {
            accumulator.statistics_count as f64
        };
        return Ok(Value::Float((accumulator.statistics_m2 / divisor).sqrt()));
    }
    let variance = numeric_statistical_variance(accumulator, sample)?;
    if variance.is_nan() {
        return Ok(Value::Decimal(variance));
    }
    let scale = variance
        .display_scale()
        .ok_or_else(|| SQLError::TypeMismatch("numeric standard deviation is not finite".into()))?;
    variance
        .sqrt_to_scale(scale)
        .map(Value::Decimal)
        .ok_or_else(|| SQLError::TypeMismatch("numeric standard deviation is undefined".into()))
}

fn numeric_statistical_variance(
    accumulator: &AggregateAccumulator,
    sample: bool,
) -> Result<DecimalValue, SQLError> {
    let count = DecimalValue::from_i128(i128::from(accumulator.statistics_count))
        .ok_or_else(|| SQLError::TypeMismatch("statistical aggregate count overflow".into()))?;
    let sum = accumulator.statistics_sum.as_ref().ok_or_else(|| {
        SQLError::Internal("numeric statistical aggregate omitted its sum".into())
    })?;
    let sum_squares = accumulator.statistics_sum_squares.as_ref().ok_or_else(|| {
        SQLError::Internal("numeric statistical aggregate omitted its square sum".into())
    })?;
    let numerator = count
        .checked_mul(sum_squares)
        .and_then(|total| {
            sum.checked_mul(sum)
                .and_then(|square| total.checked_sub(&square))
        })
        .ok_or_else(|| SQLError::TypeMismatch("numeric statistical result overflow".into()))?;
    if numerator <= DecimalValue::from_i64(0) && !accumulator.statistics_nonzero_deviation {
        return Ok(DecimalValue::from_i64(0));
    }
    let denominator = if sample {
        let count_minus_one = DecimalValue::from_i128(i128::from(accumulator.statistics_count - 1))
            .ok_or_else(|| SQLError::TypeMismatch("statistical aggregate count overflow".into()))?;
        count.checked_mul(&count_minus_one)
    } else {
        count.checked_mul(&count)
    }
    .ok_or_else(|| SQLError::TypeMismatch("numeric statistical divisor overflow".into()))?;
    let variance = numerator
        .checked_div_postgres(&denominator)
        .ok_or_else(|| SQLError::TypeMismatch("numeric statistical division failed".into()))?;
    if variance <= DecimalValue::from_i64(0) {
        return variance.checked_sub(&variance).ok_or_else(|| {
            SQLError::TypeMismatch("numeric statistical zero normalization failed".into())
        });
    }
    Ok(variance)
}

pub(in crate::sql) fn percentile_cont(
    values: &AggregateValueBuffer,
    frac: f64,
) -> Result<Option<f64>, SQLError> {
    if values.next_sequence == 0 {
        return Ok(None);
    }
    let position = frac * (values.next_sequence as f64 - 1.0);
    let low = position.floor() as u64;
    let high = position.ceil() as u64;
    let mut low_value = None;
    let mut high_value = None;
    let mut index = 0_u64;
    values.for_each_ordered(|record| {
        if index == low {
            low_value = Some(value_as_f64(&record.value)?);
        }
        if index == high {
            high_value = Some(value_as_f64(&record.value)?);
        }
        index = index
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("percentile aggregate index overflow".into()))?;
        Ok(())
    })?;
    let low_value = low_value.ok_or_else(|| {
        SQLError::Internal("percentile lower value missing from aggregate spill".into())
    })?;
    let high_value = high_value.ok_or_else(|| {
        SQLError::Internal("percentile upper value missing from aggregate spill".into())
    })?;
    let weight = position - low as f64;
    Ok(Some(low_value * (1.0 - weight) + high_value * weight))
}

pub(in crate::sql) fn percentile_disc(
    values: &AggregateValueBuffer,
    frac: f64,
) -> Result<Option<Value>, SQLError> {
    if values.next_sequence == 0 {
        return Ok(None);
    }
    let rank = ((frac * values.next_sequence as f64).ceil() as u64)
        .max(1)
        .min(values.next_sequence);
    let mut value = None;
    let mut index = 0_u64;
    values.for_each_ordered(|record| {
        index = index
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("percentile aggregate index overflow".into()))?;
        if index == rank {
            value = Some(record.value);
        }
        Ok(())
    })?;
    Ok(value)
}

pub(in crate::sql) fn mode_value(values: &AggregateValueBuffer) -> Result<Value, SQLError> {
    if values.next_sequence == 0 {
        return Ok(Value::Null);
    }
    let mut current_key = None;
    let mut current_value = Value::Null;
    let mut current_count = 0_u64;
    let mut best_value = Value::Null;
    let mut best_count = 0_u64;
    values.for_each_ordered(|record| {
        let key = distinct_key(&record.value)?;
        if current_key.as_ref().is_some_and(|current| current != &key) {
            if current_count >= best_count {
                best_count = current_count;
                best_value = current_value.clone();
            }
            current_count = 0;
        }
        if current_key.as_ref() != Some(&key) {
            current_key = Some(key);
            current_value = record.value;
        }
        current_count = current_count
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("mode aggregate count overflow".into()))?;
        Ok(())
    })?;
    if current_count >= best_count {
        best_value = current_value;
    }
    Ok(best_value)
}

/// Compute a projection's `PostgreSQL` output column name. Standalone expressions use `?column?`; repeated labels remain repeated until the final named-map compatibility boundary.
pub(in crate::sql) fn projection_label_at(proj: &ProjectionPlan) -> String {
    if let Some(a) = &proj.alias {
        return a.clone();
    }
    match &proj.expr {
        ScalarExpr::Column(c) => c.clone(),
        ScalarExpr::QualifiedColumn { column, .. } => column.clone(),
        ScalarExpr::Star | ScalarExpr::QualifiedStar(_) => "*".into(),
        ScalarExpr::Func { name, .. } => name.clone(),
        _ => "?column?".into(),
    }
}
