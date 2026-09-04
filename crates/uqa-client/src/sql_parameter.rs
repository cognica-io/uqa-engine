//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::Serialize;
use uqa_core::{TemporalValue, Value};
use uqa_sql::SQLParam;

use crate::HttpEngineError;

/// Stable data-plane representation of one UQA SQL bind parameter.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum SQLParameter {
    Null,
    Boolean {
        value: bool,
    },
    Int64 {
        value: i64,
    },
    Float64 {
        value: f64,
    },
    Text {
        value: String,
    },
    Bytes {
        hex: String,
    },
    Decimal {
        value: String,
    },
    Date {
        value: String,
    },
    Time {
        value: String,
    },
    TimeTz {
        value: String,
    },
    Timestamp {
        value: String,
    },
    TimestampTz {
        value: String,
    },
    Interval {
        value: String,
    },
    #[serde(rename = "json")]
    Json {
        value: Value,
    },
    Vector {
        value: Vec<f32>,
    },
    Tensor {
        value: Vec<Vec<f32>>,
    },
}

impl TryFrom<&SQLParam> for SQLParameter {
    type Error = HttpEngineError;

    fn try_from(parameter: &SQLParam) -> Result<Self, Self::Error> {
        match parameter {
            SQLParam::Scalar(value) | SQLParam::TypedScalar { value, .. } => {
                scalar_parameter(value)
            }
            SQLParam::Vector(value) if finite_vector(value) => Ok(Self::Vector {
                value: value.clone(),
            }),
            SQLParam::Tensor(value) if value.iter().all(|row| finite_vector(row)) => {
                Ok(Self::Tensor {
                    value: value.clone(),
                })
            }
            SQLParam::Vector(_) | SQLParam::Tensor(_) => Err(HttpEngineError::InvalidParameter),
        }
    }
}

fn scalar_parameter(value: &Value) -> Result<SQLParameter, HttpEngineError> {
    if !finite_value(value) {
        return Err(HttpEngineError::InvalidParameter);
    }
    let parameter = match value {
        Value::Null => SQLParameter::Null,
        Value::Void => SQLParameter::Text {
            value: String::new(),
        },
        Value::Bool(value) => SQLParameter::Boolean { value: *value },
        Value::Int(value) => SQLParameter::Int64 { value: *value },
        Value::Float(value) if value.is_finite() => SQLParameter::Float64 { value: *value },
        Value::Float(_) => return Err(HttpEngineError::InvalidParameter),
        Value::Str(value) => SQLParameter::Text {
            value: value.clone(),
        },
        Value::Bytes(value) => SQLParameter::Bytes {
            hex: encode_hex(value),
        },
        Value::Decimal(value) => SQLParameter::Decimal {
            value: value.to_sql_string(),
        },
        Value::Temporal(value) => temporal_parameter(value),
        Value::FixedChar(_)
        | Value::Json(_)
        | Value::JsonB(_)
        | Value::Array(_)
        | Value::List(_)
        | Value::Row(_)
        | Value::Record(_)
        | Value::Map(_) => SQLParameter::Json {
            value: value.clone(),
        },
    };
    Ok(parameter)
}

fn finite_value(value: &Value) -> bool {
    match value {
        Value::Float(value) => value.is_finite(),
        Value::Array(value) => value.elements().iter().all(finite_value),
        Value::List(values) | Value::Row(values) => values.iter().all(finite_value),
        Value::Record(values) => values.iter().all(|(_, value)| finite_value(value)),
        Value::Map(values) => values.values().all(finite_value),
        Value::Null
        | Value::Void
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Str(_)
        | Value::FixedChar(_)
        | Value::Bytes(_)
        | Value::Temporal(_)
        | Value::Decimal(_)
        | Value::Json(_)
        | Value::JsonB(_) => true,
    }
}

fn temporal_parameter(value: &TemporalValue) -> SQLParameter {
    let text = value.to_sql_string();
    match value {
        TemporalValue::Date { .. } => SQLParameter::Date { value: text },
        TemporalValue::Time { .. } => SQLParameter::Time { value: text },
        TemporalValue::TimeTz { .. } => SQLParameter::TimeTz { value: text },
        TemporalValue::Timestamp { .. } => SQLParameter::Timestamp { value: text },
        TemporalValue::TimestampTz { .. } => SQLParameter::TimestampTz { value: text },
        TemporalValue::Interval { .. } => SQLParameter::Interval { value: text },
    }
}

fn finite_vector(value: &[f32]) -> bool {
    value.iter().all(|component| component.is_finite())
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn engine_parameters_use_the_stable_wire_contract() {
        let parameters = [
            SQLParam::scalar(Value::Int(42)),
            SQLParam::scalar(Value::Void),
            SQLParam::scalar(Value::Bytes(vec![0x00, 0x0f, 0xff])),
            SQLParam::vector(vec![0.25, 0.75]),
        ];
        let encoded = parameters
            .iter()
            .map(SQLParameter::try_from)
            .map(|parameter| serde_json::to_value(parameter.unwrap()).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            encoded,
            [
                json!({"type": "int64", "value": 42}),
                json!({"type": "text", "value": ""}),
                json!({"type": "bytes", "hex": "000fff"}),
                json!({"type": "vector", "value": [0.25, 0.75]}),
            ]
        );
    }

    #[test]
    fn non_finite_parameters_fail_before_an_http_request() {
        for parameter in [
            SQLParam::scalar(Value::Float(f64::NAN)),
            SQLParam::scalar(Value::Map(std::collections::BTreeMap::from([(
                "nested".to_owned(),
                Value::List(vec![Value::Float(f64::INFINITY)]),
            )]))),
            SQLParam::vector(vec![f32::INFINITY]),
            SQLParam::tensor(vec![vec![f32::NEG_INFINITY]]),
        ] {
            assert!(matches!(
                SQLParameter::try_from(&parameter),
                Err(HttpEngineError::InvalidParameter)
            ));
        }
    }
}
