//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in aggregate finalization and percentile/statistical helpers.

use super::{
    core_value_to_json, distinct_key, value_as_f64, AggregateAccumulator, AggregateValueBuffer,
    BTreeMap, DecimalValue, ProjectionPlan, SQLError, ScalarExpr, Value,
};

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
                Value::Float(acc.integer_sum as f64 / acc.count as f64)
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
                    Value::Bytes(_) | Value::List(_) | Value::Map(_) => {
                        Err(SQLError::TypeMismatch(format!(
                            "string_agg requires a text-coercible value, got {v:?}"
                        )))
                    }
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
            Value::List(ordered_values)
        }
        "json_agg" | "jsonb_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            Value::List(ordered_values)
        }
        "json_object_agg" | "jsonb_object_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            let mut map = BTreeMap::new();
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
                map.insert(aggregate_json_key(&pair[0]), pair[1].clone());
            }
            if map.is_empty() {
                Value::Null
            } else {
                Value::Map(map)
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
            statistical_value(
                acc,
                (acc.statistics_m2 / (acc.statistics_count as f64 - 1.0)).sqrt(),
            )
        }
        "stddev_pop" => {
            if acc.statistics_count == 0 {
                return Ok(Value::Null);
            }
            statistical_value(
                acc,
                (acc.statistics_m2 / acc.statistics_count as f64).sqrt(),
            )
        }
        "variance" | "var_samp" => {
            if acc.statistics_count < 2 {
                return Ok(Value::Null);
            }
            statistical_value(acc, acc.statistics_m2 / (acc.statistics_count as f64 - 1.0))
        }
        "var_pop" => {
            if acc.statistics_count == 0 {
                return Ok(Value::Null);
            }
            statistical_value(acc, acc.statistics_m2 / acc.statistics_count as f64)
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
        Value::List(_) | Value::Map(_) => serde_json::to_string(&core_value_to_json(value))
            .unwrap_or_else(|_| format!("{value:?}")),
    }
}

/// Statistical aggregates (`variance`, `stddev_*`) return `numeric`
/// for integer / numeric inputs in `PostgreSQL` (rendering with a
/// decimal point, e.g. `1.00000...`), and `double precision` only for
/// float inputs.
pub(in crate::sql) fn statistical_value(
    accumulator: &AggregateAccumulator,
    computed: f64,
) -> Value {
    if accumulator.statistics_has_float || !computed.is_finite() {
        return Value::Float(computed);
    }
    uqa_core::DecimalValue::parse(&format!("{computed:.16}"))
        .map_or(Value::Float(computed), Value::Decimal)
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

/// Compute a projection's output column name. `PostgreSQL` reports
/// standalone expressions as `?column?`; `projection_columns` adds a
/// suffix when the row map needs unique keys.
pub(in crate::sql) fn projection_label_at(proj: &ProjectionPlan) -> String {
    if let Some(a) = &proj.alias {
        return a.clone();
    }
    match &proj.expr {
        ScalarExpr::Column(c) => c.clone(),
        ScalarExpr::QualifiedColumn { column, .. } => column.clone(),
        ScalarExpr::Star => "*".into(),
        ScalarExpr::Func { name, .. } => name.clone(),
        _ => "?column?".into(),
    }
}
