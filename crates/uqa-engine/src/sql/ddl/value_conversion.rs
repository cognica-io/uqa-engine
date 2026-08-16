//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared-column coercion, temporal conversion, and JSON bridges.

use super::{
    ddl_storage_error, index_vectors_for_type, value_to_tensor, value_to_vector, BTreeMap,
    ColumnType, DecimalValue, Engine, RowUpdateVectors, SQLError, TemporalValue, Value,
};
use uqa_core::ArrayValue;
use uqa_sql::ast::Expr;

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

pub(super) fn coerce_json_value(value: Value, jsonb: bool) -> Result<Value, SQLError> {
    uqa_sql::expr::cast_value(&value, if jsonb { "jsonb" } else { "json" })
}

pub(super) fn rewrite_column_values_to_type(
    engine: &Engine,
    table: &str,
    column: &str,
    source_ty: &ColumnType,
    target_ty: &ColumnType,
    using: Option<&Expr>,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let schema = uqa_execution::RowSchema::with_types(
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect(),
        definitions
            .iter()
            .map(|definition| {
                Some(if definition.name == column {
                    source_ty.clone()
                } else {
                    definition.ty.clone()
                })
            })
            .collect(),
    );
    for doc_id in engine.table_doc_ids(table)? {
        let Some(doc) = engine.get_document(table, doc_id)? else {
            continue;
        };
        let converted = if let Some(expression) = using {
            let value = crate::sql::scalar::eval_lowered_expression_with_schema(
                engine,
                expression,
                &doc,
                &schema,
                &[],
            )?;
            convert_value_to_column_type(value, target_ty)?
        } else {
            let Some(value) = doc.get(column).cloned() else {
                continue;
            };
            convert_declared_value_to_column_type(value, source_ty, target_ty)?
        };
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), converted.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if matches!(target_ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
            vectors.insert(
                column.to_string(),
                index_vectors_for_type(&converted, target_ty)?,
            );
        }
        engine.update_document_fields_with_vector_values(table, doc_id, updates, vectors)?;
    }
    Ok(())
}

fn convert_declared_value_to_column_type(
    value: Value,
    source_ty: &ColumnType,
    target_ty: &ColumnType,
) -> Result<Value, SQLError> {
    match (source_ty, target_ty) {
        (ColumnType::Domain { base, .. }, target) => {
            convert_declared_value_to_column_type(value, base, target)
        }
        (source, ColumnType::Domain { base, .. }) => {
            convert_declared_value_to_column_type(value, source, base)
        }
        (ColumnType::Array(source), ColumnType::Array(target)) => {
            let Value::Array(array) = value else {
                return Err(SQLError::TypeMismatch(format!(
                    "cannot cast a non-array value to {}[]",
                    column_type_name(target)
                )));
            };
            let source = array_scalar_type(source);
            let target = array_scalar_type(target);
            let converted = convert_declared_array_elements(array.elements(), source, target)?;
            ArrayValue::with_lower_bounds(converted, array.lower_bounds().to_vec())
                .map(Value::Array)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "multidimensional arrays must have matching dimensions".into(),
                    )
                })
        }
        (source, ColumnType::Oid)
            if matches!(
                source,
                ColumnType::SmallInteger
                    | ColumnType::Integer
                    | ColumnType::BigInteger
                    | ColumnType::Oid
                    | ColumnType::Regproc
                    | ColumnType::Regclass
                    | ColumnType::Regnamespace
                    | ColumnType::Regtype
            ) =>
        {
            uqa_sql::expr::cast_value_from(&value, "oid", Some(column_type_name(source)))
        }
        (ColumnType::Xid, ColumnType::Xid) => Ok(value),
        (ColumnType::Bytea, ColumnType::Bytea) => Ok(value),
        (_, ColumnType::Oid | ColumnType::Xid | ColumnType::Bytea) => {
            Err(SQLError::TypeMismatch(format!(
                "column cannot be cast automatically from type {} to type {}",
                column_type_name(source_ty),
                column_type_name(target_ty)
            )))
        }
        _ => convert_value_to_column_type(value, target_ty),
    }
}

pub(crate) fn convert_value_to_column_type(
    value: Value,
    ty: &ColumnType,
) -> Result<Value, SQLError> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    match ty {
        ColumnType::SmallInteger => uqa_sql::expr::cast_value(&value, "smallint"),
        ColumnType::Integer => uqa_sql::expr::cast_value(&value, "integer"),
        ColumnType::BigInteger => uqa_sql::expr::cast_value(&value, "bigint"),
        ColumnType::Oid | ColumnType::Xid => {
            let Value::Int(value) = uqa_sql::expr::cast_value(&value, "bigint")? else {
                unreachable!("bigint cast returned a non-integer value")
            };
            u32::try_from(value)
                .map(|value| Value::Int(i64::from(value)))
                .map_err(|_| {
                    SQLError::TypeMismatch(format!(
                        "value {value} is out of range for type {}",
                        column_type_name(ty)
                    ))
                })
        }
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
        ColumnType::Name => uqa_sql::expr::cast_value(&value, "name"),
        ColumnType::Uuid => uqa_sql::expr::cast_value(&value, "uuid"),
        ColumnType::Varchar(None) => Ok(Value::Str(value_to_text(&value))),
        ColumnType::Varchar(Some(length)) => convert_varying_character(value, *length),
        ColumnType::Bpchar => Ok(Value::FixedChar(value_to_text(&value))),
        ColumnType::Character(length) => {
            let length = usize::try_from(*length).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "character length {length} exceeds the platform addressable range"
                ))
            })?;
            let text = value_to_text(&value);
            let char_count = text.chars().count();
            let significant = if char_count > length {
                let retained = text.chars().take(length).collect::<String>();
                let discarded = text.chars().skip(length).collect::<String>();
                if !discarded.chars().all(|character| character == ' ') {
                    return Err(SQLError::TypeMismatch(format!(
                        "value too long for type character({length})"
                    )));
                }
                retained
            } else {
                text
            };
            let padding = length.saturating_sub(significant.chars().count());
            let mut padded = significant;
            padded.extend(std::iter::repeat_n(' ', padding));
            Ok(Value::FixedChar(padded))
        }
        ColumnType::Real | ColumnType::DoublePrecision => match value {
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
        ColumnType::Json => coerce_json_value(value, false),
        ColumnType::JsonB => coerce_json_value(value, true),
        ColumnType::Bytea => Ok(match value {
            Value::Bytes(_) => value,
            Value::Str(s) => Value::Bytes(s.into_bytes()),
            other => Value::Bytes(value_to_text(&other).into_bytes()),
        }),
        ColumnType::InternalChar => {
            let text = value_to_text(&value);
            if text.len() == 1 {
                Ok(Value::Str(text))
            } else {
                Err(SQLError::TypeMismatch(format!(
                    "value `{text}` must be exactly one byte for type \"char\""
                )))
            }
        }
        ColumnType::Regproc
        | ColumnType::Regclass
        | ColumnType::Regnamespace
        | ColumnType::Regtype
        | ColumnType::PgNodeTree
        | ColumnType::AclItem => Ok(match value {
            Value::Int(_) | Value::Str(_) => value,
            other => Value::Str(value_to_text(&other)),
        }),
        ColumnType::Int2Vector => convert_value_to_column_type(
            value,
            &ColumnType::Array(Box::new(ColumnType::SmallInteger)),
        ),
        ColumnType::OidVector => {
            convert_value_to_column_type(value, &ColumnType::Array(Box::new(ColumnType::Oid)))
        }
        ColumnType::AnyArray => match value {
            Value::Array(_) => Ok(value),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to anyarray"
            ))),
        },
        ColumnType::Record => match value {
            Value::Record(_) => Ok(value),
            Value::Row(values) => Ok(Value::Record(
                values
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| (format!("f{}", index + 1), value))
                    .collect(),
            )),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to record"
            ))),
        },
        ColumnType::Array(element_type) => {
            let array = match value {
                Value::Array(array) => array,
                Value::List(elements) => ArrayValue::try_new(elements).ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "multidimensional arrays must have matching dimensions".into(),
                    )
                })?,
                Value::Str(text) => uqa_sql::expr::parse_pg_array_literal(&text)?,
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot cast {other:?} to {}[]",
                        column_type_name(element_type)
                    )))
                }
            };
            let converted = convert_array_elements(array.elements(), element_type)?;
            ArrayValue::with_lower_bounds(converted, array.lower_bounds().to_vec())
                .map(Value::Array)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "multidimensional arrays must have matching dimensions".into(),
                    )
                })
        }
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimeTz
        | ColumnType::Timestamp
        | ColumnType::TimestampTz
        | ColumnType::Interval => convert_temporal_value(value, ty),
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
        ColumnType::Domain { base, .. } => convert_value_to_column_type(value, base),
    }
}

fn convert_array_elements(
    elements: &[Value],
    element_type: &ColumnType,
) -> Result<Vec<Value>, SQLError> {
    let element_type = array_scalar_type(element_type);
    elements
        .iter()
        .cloned()
        .map(|element| match element {
            Value::List(nested) => convert_array_elements(&nested, element_type).map(Value::List),
            scalar => convert_value_to_column_type(scalar, element_type),
        })
        .collect()
}

fn convert_declared_array_elements(
    elements: &[Value],
    source_type: &ColumnType,
    target_type: &ColumnType,
) -> Result<Vec<Value>, SQLError> {
    elements
        .iter()
        .cloned()
        .map(|element| match element {
            Value::List(nested) => {
                convert_declared_array_elements(&nested, source_type, target_type).map(Value::List)
            }
            scalar => convert_declared_value_to_column_type(scalar, source_type, target_type),
        })
        .collect()
}

fn array_scalar_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Array(element) = ty {
        ty = element;
    }
    ty
}

fn convert_varying_character(value: Value, length: u32) -> Result<Value, SQLError> {
    let length = usize::try_from(length).map_err(|_| {
        SQLError::TypeMismatch(format!(
            "character varying length {length} exceeds the platform addressable range"
        ))
    })?;
    let text = value_to_text(&value);
    if text.chars().count() <= length {
        return Ok(Value::Str(text));
    }
    let retained = text.chars().take(length).collect::<String>();
    let discarded = text.chars().skip(length).collect::<String>();
    if discarded.chars().all(|character| character == ' ') {
        Ok(Value::Str(retained))
    } else {
        Err(SQLError::TypeMismatch(format!(
            "value too long for type character varying({length})"
        )))
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

pub(in crate::sql) fn column_type_name(ty: &ColumnType) -> &str {
    match ty {
        ColumnType::SmallInteger => "smallint",
        ColumnType::Integer => "integer",
        ColumnType::BigInteger => "bigint",
        ColumnType::Oid => "oid",
        ColumnType::Xid => "xid",
        ColumnType::Boolean => "boolean",
        ColumnType::Text => "text",
        ColumnType::Name => "name",
        ColumnType::Uuid => "uuid",
        ColumnType::Varchar(_) => "character varying",
        ColumnType::Bpchar => "character",
        ColumnType::Character(_) => "character",
        ColumnType::Real => "real",
        ColumnType::DoublePrecision => "double precision",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Json => "json",
        ColumnType::JsonB => "jsonb",
        ColumnType::Bytea => "bytea",
        ColumnType::InternalChar => "\"char\"",
        ColumnType::Regproc => "regproc",
        ColumnType::Regclass => "regclass",
        ColumnType::Regnamespace => "regnamespace",
        ColumnType::Regtype => "regtype",
        ColumnType::PgNodeTree => "pg_node_tree",
        ColumnType::AclItem => "aclitem",
        ColumnType::Int2Vector => "int2vector",
        ColumnType::OidVector => "oidvector",
        ColumnType::AnyArray => "anyarray",
        ColumnType::Record => "record",
        ColumnType::Array(_) => "array",
        ColumnType::Date => "date",
        ColumnType::Time => "time",
        ColumnType::TimeTz => "time with time zone",
        ColumnType::Timestamp => "timestamp",
        ColumnType::TimestampTz => "timestamp with time zone",
        ColumnType::Interval => "interval",
        ColumnType::Vector(_) => "vector",
        ColumnType::Tensor(_) => "tensor",
        ColumnType::Domain { name, .. } => name,
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
    uqa_sql::expr::cast_value(&value, column_type_name(ty))
}

pub(in crate::sql) fn value_to_text(value: &Value) -> String {
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
        Value::Array(array) => uqa_sql::expr::array_value_to_string(array),
        Value::List(_) | Value::Map(_) => serde_json::to_string(&core_value_to_json(value))
            .unwrap_or_else(|_| format!("{value:?}")),
        Value::Row(_) | Value::Record(_) => uqa_sql::expr::value_to_string(value),
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
        Value::FixedChar(s) => serde_json::Value::String(s.trim_end_matches(' ').to_string()),
        Value::Bytes(bytes) => serde_json::Value::String(String::from_utf8_lossy(bytes).into()),
        Value::Temporal(t) => serde_json::Value::String(t.to_sql_string()),
        Value::Json(text) | Value::JsonB(text) => {
            serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.clone()))
        }
        Value::Array(array) => {
            serde_json::Value::Array(array.elements().iter().map(core_value_to_json).collect())
        }
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(core_value_to_json).collect())
        }
        Value::Row(values) => serde_json::Value::Object(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (format!("f{}", index + 1), core_value_to_json(value)))
                .collect(),
        ),
        Value::Record(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), core_value_to_json(value)))
                .collect(),
        ),
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
        Value::Json(s) | Value::JsonB(s) | Value::Str(s) => {
            serde_json::from_str::<serde_json::Value>(s)
                .map_err(|e| SQLError::TypeMismatch(format!("{name}: invalid JSON: {e}")))
        }
        other => Ok(core_value_to_json(other)),
    }
}
