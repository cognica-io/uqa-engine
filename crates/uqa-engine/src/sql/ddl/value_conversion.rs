//! Declared-column coercion, temporal conversion, and JSON bridges.

use super::{
    ddl_storage_error, index_vectors_for_type, value_to_tensor, value_to_vector, BTreeMap,
    ColumnType, DecimalValue, Engine, RowUpdateVectors, SQLError, TemporalValue, Value,
};

/// Coerce a write value to fit the column's declared type.
pub(in crate::sql) fn coerce_to_column_type(
    engine: &Engine,
    table: &str,
    column: &str,
    value: Value,
) -> Result<Value, SQLError> {
    let cols = match engine
        .try_describe_table(table)
        .map_err(|err| ddl_storage_error("column type coercion", err))?
    {
        Some(c) => c,
        None => return Ok(value),
    };
    let Some(def) = cols.iter().find(|c| c.name == column) else {
        return Ok(value);
    };
    convert_value_to_column_type(value, &def.ty)
}

pub(super) fn coerce_json_value(value: Value) -> Result<Value, SQLError> {
    match value {
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(&s)
            .map(json_to_core_value)
            .map_err(|error| {
                SQLError::TypeMismatch(format!("cannot cast string to JSON: {error}"))
            }),
        other => Ok(other),
    }
}

pub(super) fn float_to_integer(value: f64) -> Result<i64, SQLError> {
    // `i64::MAX as f64` rounds to 2^63, which itself is outside the range.
    const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    const I64_LOWER_INCLUSIVE: f64 = -9_223_372_036_854_775_808.0;
    if !value.is_finite() || !(I64_LOWER_INCLUSIVE..I64_UPPER_EXCLUSIVE).contains(&value) {
        return Err(SQLError::TypeMismatch(format!(
            "cannot cast {value:?} to integer: value is outside BIGINT range"
        )));
    }
    Ok(value as i64)
}

pub(super) fn rewrite_column_values_to_type(
    engine: &Engine,
    table: &str,
    column: &str,
    ty: &ColumnType,
) -> Result<(), SQLError> {
    for doc_id in engine.table_doc_ids(table)? {
        let Some(doc) = engine.get_document(table, doc_id)? else {
            continue;
        };
        let Some(value) = doc.get(column).cloned() else {
            continue;
        };
        let converted = convert_value_to_column_type(value, ty)?;
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), converted.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
            vectors.insert(column.to_string(), index_vectors_for_type(&converted, ty)?);
        }
        engine.update_document_fields_with_vector_values(table, doc_id, updates, vectors)?;
    }
    Ok(())
}

pub(crate) fn convert_value_to_column_type(
    value: Value,
    ty: &ColumnType,
) -> Result<Value, SQLError> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    match ty {
        ColumnType::Integer => match value {
            Value::Int(_) => Ok(value),
            Value::Float(f) => float_to_integer(f).map(Value::Int),
            Value::Decimal(d) => d
                .to_i64_trunc()
                .map(Value::Int)
                .ok_or_else(|| SQLError::TypeMismatch("cannot cast decimal to integer".into())),
            Value::Bool(b) => Ok(Value::Int(i64::from(b))),
            Value::Str(s) => s
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|e| SQLError::TypeMismatch(format!("cannot cast `{s}` to integer: {e}"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to integer"
            ))),
        },
        ColumnType::Boolean => match value {
            Value::Bool(_) => Ok(value),
            Value::Str(text) => parse_boolean_text(&text)
                .map(Value::Bool)
                .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast `{text}` to boolean"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to boolean"
            ))),
        },
        ColumnType::Text => Ok(Value::Str(value_to_text(&value))),
        ColumnType::Real => match value {
            Value::Float(_) => Ok(value),
            Value::Int(i) => Ok(Value::Float(i as f64)),
            Value::Decimal(d) => d
                .to_f64()
                .map(Value::Float)
                .ok_or_else(|| SQLError::TypeMismatch("cannot cast decimal to real".into())),
            Value::Bool(b) => Ok(Value::Float(if b { 1.0 } else { 0.0 })),
            Value::Str(s) => s
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|e| SQLError::TypeMismatch(format!("cannot cast `{s}` to real: {e}"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to real"
            ))),
        },
        ColumnType::Numeric { precision, scale } => {
            let decimal = match value {
                Value::Decimal(d) => d,
                Value::Int(i) => DecimalValue::from_i64(i),
                Value::Float(f) => DecimalValue::from_f64_lossy(f).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot cast {f:?} to numeric"))
                })?,
                Value::Bool(b) => DecimalValue::from_bool(b),
                Value::Str(s) => DecimalValue::parse(&s).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot cast `{s}` to numeric"))
                })?,
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot cast {other:?} to numeric"
                    )));
                }
            };
            let rounded = match scale {
                Some(s) => decimal.round_to_scale(*s).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot round numeric to scale {s}"))
                })?,
                None => decimal,
            };
            if let Some(precision) = precision {
                let scale = scale.unwrap_or(0);
                if !rounded.fits_precision(*precision, scale) {
                    return Err(SQLError::TypeMismatch(format!(
                        "numeric field overflow: value {} exceeds precision {precision}, scale {scale}",
                        rounded.to_sql_string()
                    )));
                }
            }
            Ok(Value::Decimal(rounded))
        }
        ColumnType::Json | ColumnType::JsonB => coerce_json_value(value),
        ColumnType::Bytea => Ok(match value {
            Value::Bytes(_) => value,
            Value::Str(s) => Value::Bytes(s.into_bytes()),
            other => Value::Bytes(value_to_text(&other).into_bytes()),
        }),
        ColumnType::Array(element_type) => {
            let items = match value {
                Value::List(items) => items,
                Value::Str(text) => uqa_sql::expr::parse_pg_array_literal(&text)?,
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot cast {other:?} to {}[]",
                        column_type_name(element_type)
                    )))
                }
            };
            let converted = items
                .into_iter()
                .map(|item| convert_value_to_column_type(item, element_type))
                .collect::<Result<Vec<_>, _>>()?;
            uqa_sql::expr::array_dimensions(&converted)?;
            Ok(Value::List(converted))
        }
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimeTz
        | ColumnType::Timestamp
        | ColumnType::TimestampTz => convert_temporal_value(value, ty),
        ColumnType::Vector(dim) => {
            let vector = value_to_vector(&value)?;
            validate_vector_dimensions(*dim, vector.len())?;
            Ok(vector_to_value(vector))
        }
        ColumnType::Tensor(dim) => {
            let tensor = value_to_tensor(&value)?;
            for vector in &tensor {
                validate_vector_dimensions(*dim, vector.len())?;
            }
            Ok(Value::List(
                tensor.into_iter().map(vector_to_value).collect(),
            ))
        }
    }
}

fn vector_to_value(vector: Vec<f32>) -> Value {
    Value::List(
        vector
            .into_iter()
            .map(|value| Value::Float(f64::from(value)))
            .collect(),
    )
}

pub(crate) fn validate_vector_dimensions(expected: u32, actual: usize) -> Result<(), SQLError> {
    let expected = usize::try_from(expected).map_err(|_| {
        SQLError::TypeMismatch(format!(
            "declared vector dimension {expected} exceeds the platform usize range"
        ))
    })?;
    if actual == expected {
        Ok(())
    } else {
        Err(SQLError::VectorDimMismatch { expected, actual })
    }
}

pub(in crate::sql) fn column_type_name(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Integer => "integer",
        ColumnType::Boolean => "boolean",
        ColumnType::Text => "text",
        ColumnType::Real => "real",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Json => "json",
        ColumnType::JsonB => "jsonb",
        ColumnType::Bytea => "bytea",
        ColumnType::Array(_) => "array",
        ColumnType::Date => "date",
        ColumnType::Time => "time",
        ColumnType::TimeTz => "time with time zone",
        ColumnType::Timestamp => "timestamp",
        ColumnType::TimestampTz => "timestamp with time zone",
        ColumnType::Vector(_) => "vector",
        ColumnType::Tensor(_) => "tensor",
    }
}

fn parse_boolean_text(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "on" | "1" => Some(true),
        "false" | "f" | "no" | "n" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn convert_temporal_value(value: Value, ty: &ColumnType) -> Result<Value, SQLError> {
    match value {
        Value::Temporal(temporal) => coerce_temporal_kind(temporal, ty)
            .map(Value::Temporal)
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "cannot cast temporal value to {}",
                    column_type_name(ty)
                ))
            }),
        other => {
            let text = value_to_text(&other);
            let parsed = parse_temporal_text_for_type(&text, ty);
            parsed.map(Value::Temporal).ok_or_else(|| {
                SQLError::TypeMismatch(format!("cannot cast `{text}` to {}", column_type_name(ty)))
            })
        }
    }
}

fn coerce_temporal_kind(value: TemporalValue, ty: &ColumnType) -> Option<TemporalValue> {
    match (ty, value) {
        (ColumnType::Date, value @ TemporalValue::Date { .. })
        | (ColumnType::Time, value @ TemporalValue::Time { .. })
        | (ColumnType::TimeTz, value @ TemporalValue::TimeTz { .. })
        | (ColumnType::Timestamp, value @ TemporalValue::Timestamp { .. })
        | (ColumnType::TimestampTz, value @ TemporalValue::TimestampTz { .. }) => Some(value),
        (ColumnType::Timestamp, TemporalValue::TimestampTz { micros }) => {
            Some(TemporalValue::Timestamp { micros })
        }
        (ColumnType::TimestampTz, TemporalValue::Timestamp { micros }) => {
            Some(TemporalValue::TimestampTz { micros })
        }
        _ => None,
    }
}

fn parse_temporal_text_for_type(text: &str, ty: &ColumnType) -> Option<TemporalValue> {
    match ty {
        ColumnType::Date => TemporalValue::parse_date(text),
        ColumnType::Time => TemporalValue::parse_time(text),
        ColumnType::TimeTz => TemporalValue::parse_time_tz(text),
        ColumnType::Timestamp => TemporalValue::parse_timestamp(text).or_else(|| {
            TemporalValue::parse_timestamp_tz(text).and_then(|value| match value {
                TemporalValue::TimestampTz { micros } => Some(TemporalValue::Timestamp { micros }),
                _ => None,
            })
        }),
        ColumnType::TimestampTz => TemporalValue::parse_timestamp_tz(text),
        _ => None,
    }
}

pub(in crate::sql) fn value_to_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) => s.clone(),
        Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::List(_) | Value::Map(_) => serde_json::to_string(&core_value_to_json(value))
            .unwrap_or_else(|_| format!("{value:?}")),
    }
}

pub(in crate::sql) fn json_to_core_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(d) = DecimalValue::parse(&n.to_string()) {
                Value::Decimal(d)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::Str(s),
        serde_json::Value::Array(items) => {
            Value::List(items.into_iter().map(json_to_core_value).collect())
        }
        serde_json::Value::Object(obj) => {
            if let Ok(temporal) =
                serde_json::from_value::<TemporalValue>(serde_json::Value::Object(obj.clone()))
            {
                return Value::Temporal(temporal);
            }
            Value::Map(
                obj.into_iter()
                    .map(|(k, v)| (k, json_to_core_value(v)))
                    .collect(),
            )
        }
    }
}

pub(in crate::sql) fn core_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f).map_or_else(
            || {
                let label = if f.is_nan() {
                    "NaN"
                } else if f.is_sign_positive() {
                    "Infinity"
                } else {
                    "-Infinity"
                };
                serde_json::Value::String(label.to_string())
            },
            serde_json::Value::Number,
        ),
        Value::Decimal(d) => d
            .to_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(
                || serde_json::Value::String(d.to_sql_string()),
                serde_json::Value::Number,
            ),
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or_else(|_| serde_json::Value::String(s.clone())),
        Value::Bytes(bytes) => serde_json::Value::String(String::from_utf8_lossy(bytes).into()),
        Value::Temporal(t) => serde_json::Value::String(t.to_sql_string()),
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(core_value_to_json).collect())
        }
        Value::Map(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), core_value_to_json(v)))
                .collect(),
        ),
    }
}

pub(in crate::sql) fn json_table_value_to_text(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Bool(b) => Value::Str(b.to_string()),
        serde_json::Value::Number(n) => Value::Str(n.to_string()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Value::Str(value.to_string()),
    }
}

pub(in crate::sql) fn json_table_arg(
    value: &Value,
    name: &str,
) -> Result<serde_json::Value, SQLError> {
    match value {
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(s)
            .map_err(|e| SQLError::TypeMismatch(format!("{name}: invalid JSON: {e}"))),
        other => Ok(core_value_to_json(other)),
    }
}
