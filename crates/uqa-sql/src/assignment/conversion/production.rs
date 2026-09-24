//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assignment conversion preserves admitted inputs while their replacements are produced.

use super::{
    array_scalar_type, column_type_name, validate_vector_dimensions, ColumnType, SQLError,
};
use crate::expr::{
    cast_value_from_with_control, parse_pg_array_literal_with_control,
    value_to_tensor_with_control, value_to_text_with_control, value_to_vector_with_control,
};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionString, ProductionVec},
    ArrayValue, DecimalValue, Value,
};

type Result<T> = std::result::Result<T, SQLError>;

#[expect(
    clippy::too_many_lines,
    reason = "assignment matrix preserves conversion and validation order"
)]
pub fn convert_value_to_column_type_with_control(
    value: Produced<Value>,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (value, memory) = value.into_parts();
    let value = control.finish(value, memory)?;
    if matches!(&*value, Value::Null) {
        return Ok(value);
    }
    match ty {
        ColumnType::Named(name) => Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type \"{name}\" does not exist"),
        }),
        ColumnType::SmallInteger => cast_value_from_with_control(&value, "smallint", None, control),
        ColumnType::Integer => cast_value_from_with_control(&value, "integer", None, control),
        ColumnType::BigInteger => cast_value_from_with_control(&value, "bigint", None, control),
        ColumnType::Oid | ColumnType::Xid => {
            let converted = cast_value_from_with_control(&value, "bigint", None, control)?;
            let Value::Int(number) = &*converted else {
                unreachable!("bigint cast returned a non-integer value");
            };
            u32::try_from(*number).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "value {number} is out of range for type {}",
                    column_type_name(ty)
                ))
            })?;
            Ok(converted)
        }
        ColumnType::Boolean => match &*value {
            Value::Bool(_) => Ok(value),
            Value::Str(text) => {
                let boolean = parse_boolean_text(text).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot cast `{text}` to boolean"))
                })?;
                Ok(control.finish(Value::Bool(boolean), control.empty_reservation())?)
            }
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to boolean"
            ))),
        },
        ColumnType::Void => Ok(control.finish(Value::Void, control.empty_reservation())?),
        ColumnType::Text | ColumnType::RefCursor | ColumnType::Varchar(None) => {
            text_value(value_to_text_with_control(&value, control)?, false, control)
        }
        ColumnType::Name => cast_value_from_with_control(&value, "name", None, control),
        ColumnType::Uuid => cast_value_from_with_control(&value, "uuid", None, control),
        ColumnType::Varchar(Some(length)) => character_value(&value, *length, false, control),
        ColumnType::Bpchar => {
            text_value(value_to_text_with_control(&value, control)?, true, control)
        }
        ColumnType::Character(length) => character_value(&value, *length, true, control),
        ColumnType::Real => cast_value_from_with_control(&value, "real", None, control),
        ColumnType::DoublePrecision => {
            cast_value_from_with_control(&value, "double precision", None, control)
        }
        ColumnType::Numeric { precision, scale } => {
            numeric_value(value, *precision, *scale, control)
        }
        ColumnType::Json => cast_value_from_with_control(&value, "json", None, control),
        ColumnType::JsonB => cast_value_from_with_control(&value, "jsonb", None, control),
        ColumnType::Bytea => match &*value {
            Value::Bytes(_) => Ok(value),
            Value::Str(_) => {
                let (Value::Str(text), memory) = value.into_parts() else {
                    unreachable!();
                };
                Ok(control.finish(Value::Bytes(text.into_bytes()), memory)?)
            }
            _ => {
                let (text, memory) = value_to_text_with_control(&value, control)?.into_parts();
                Ok(control.finish(Value::Bytes(text.into_bytes()), memory)?)
            }
        },
        ColumnType::InternalChar => {
            let text = value_to_text_with_control(&value, control)?;
            if text.len() != 1 {
                return Err(SQLError::TypeMismatch(format!(
                    "value `{}` must be exactly one byte for type \"char\"",
                    text.as_str()
                )));
            }
            text_value(text, false, control)
        }
        ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regnamespace
        | ColumnType::Regtype
        | ColumnType::PgNodeTree
        | ColumnType::AclItem => {
            if matches!(&*value, Value::Int(_) | Value::Str(_)) {
                Ok(value)
            } else {
                text_value(value_to_text_with_control(&value, control)?, false, control)
            }
        }
        ColumnType::Regrole => match &*value {
            Value::Int(number) => {
                u32::try_from(*number).map_err(|_| {
                    SQLError::TypeMismatch(format!(
                        "value {number} is out of range for type regrole"
                    ))
                })?;
                Ok(value)
            }
            Value::Str(_) | Value::FixedChar(_) => Err(SQLError::Internal(
                "regrole name conversion requires catalog resolution".into(),
            )),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to regrole"
            ))),
        },
        ColumnType::Int2Vector => array_value(value, &ColumnType::SmallInteger, control),
        ColumnType::OidVector => array_value(value, &ColumnType::Oid, control),
        ColumnType::AnyArray => {
            if matches!(&*value, Value::Array(_)) {
                Ok(value)
            } else {
                Err(SQLError::TypeMismatch(format!(
                    "cannot cast {:?} to anyarray",
                    &*value
                )))
            }
        }
        ColumnType::Record => match &*value {
            Value::Record(_) => Ok(value),
            Value::Row(_) => record_value(value, control),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to record"
            ))),
        },
        ColumnType::Array(element) => array_value(value, element, control),
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimePrecision(_)
        | ColumnType::TimeTz
        | ColumnType::TimeTzPrecision(_)
        | ColumnType::Timestamp
        | ColumnType::TimestampPrecision(_)
        | ColumnType::TimestampTz
        | ColumnType::TimestampTzPrecision(_)
        | ColumnType::Interval
        | ColumnType::IntervalWithFields { .. } => {
            let name = ty.sql_name_with_control(control)?;
            cast_value_from_with_control(&value, &name, None, control)
        }
        ColumnType::Range(subtype) => {
            cast_value_from_with_control(&value, subtype.range_name(), None, control)
        }
        ColumnType::Multirange(subtype) => {
            cast_value_from_with_control(&value, subtype.multirange_name(), None, control)
        }
        ColumnType::Vector(dimensions) => {
            let vector = value_to_vector_with_control(&value, control)?;
            validate_vector_dimensions(*dimensions, vector.len())?;
            vector_value(&vector, control)
        }
        ColumnType::Tensor(dimensions) => {
            let tensor = value_to_tensor_with_control(&value, control)?;
            for vector in &*tensor {
                control.check()?;
                validate_vector_dimensions(*dimensions, vector.len())?;
            }
            let mut output = ProductionVec::new(*control);
            output.reserve(tensor.len())?;
            for vector in &*tensor {
                output.push_produced(vector_value(vector, control)?)?;
            }
            let (values, memory) = output.finish()?.into_parts();
            Ok(control.finish(Value::List(values), memory)?)
        }
        ColumnType::Domain { base, .. } => {
            convert_value_to_column_type_with_control(value, base, control)
        }
    }
}

fn numeric_value(
    value: Produced<Value>,
    precision: Option<u32>,
    scale: Option<i32>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let decimal = match &*value {
        Value::Decimal(_) => {
            let (Value::Decimal(value), memory) = value.into_parts() else {
                unreachable!();
            };
            control.finish(value, memory)?
        }
        Value::Int(number) => DecimalValue::from_i64_with_control(*number, control)?,
        Value::Bool(boolean) => DecimalValue::from_i64_with_control(i64::from(*boolean), control)?,
        Value::Float(number) => DecimalValue::from_f64_lossy_with_control(*number, control)?
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {number:?} to numeric")))?,
        Value::Str(text) => DecimalValue::parse_with_control(text, control)?
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast `{text}` to numeric")))?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to numeric"
            )))
        }
    };
    let rounded = match scale {
        Some(scale) => decimal
            .round_to_scale_with_control(scale, control)?
            .ok_or_else(|| {
                SQLError::TypeMismatch(format!("cannot round numeric to scale {scale}"))
            })?,
        None => decimal,
    };
    if let Some(precision) = precision {
        let scale = scale.unwrap_or(0);
        if !rounded.fits_precision_with_control(precision, scale, control)? {
            return Err(SQLError::TypeMismatch(format!(
                "numeric field overflow: value {} exceeds precision {precision}, scale {scale}",
                rounded.to_sql_string()
            )));
        }
    }
    let (decimal, memory) = rounded.into_parts();
    Ok(control.finish(Value::Decimal(decimal), memory)?)
}

fn text_value(
    text: Produced<String>,
    fixed: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (text, memory) = text.into_parts();
    Ok(control.finish(
        if fixed {
            Value::FixedChar(text)
        } else {
            Value::Str(text)
        },
        memory,
    )?)
}

fn character_value(
    value: &Value,
    length: u32,
    fixed: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let name = if fixed {
        "character"
    } else {
        "character varying"
    };
    let length = usize::try_from(length).map_err(|_| {
        SQLError::TypeMismatch(format!(
            "{name} length {length} exceeds the platform addressable range"
        ))
    })?;
    let text = value_to_text_with_control(value, control)?;
    let mut count = 0;
    let mut end = 0;
    for (index, character) in text.char_indices() {
        control.check()?;
        if count < length {
            count += 1;
            end = index + character.len_utf8();
        } else if character != ' ' {
            return Err(SQLError::Routine {
                sqlstate: "22001".into(),
                message: format!("value too long for type {name}({length})"),
            });
        }
    }
    let mut output = ProductionString::from_produced(text, *control)?;
    output.truncate(end)?;
    if fixed {
        for _ in count..length {
            output.push(' ')?;
        }
    }
    text_value(output.finish()?, fixed, control)
}

fn array_value(
    value: Produced<Value>,
    element: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let array = match &*value {
        Value::Array(_) => {
            let (Value::Array(array), memory) = value.into_parts() else {
                unreachable!();
            };
            control.finish(array, memory)?
        }
        Value::List(_) => {
            let (Value::List(elements), memory) = value.into_parts() else {
                unreachable!();
            };
            ArrayValue::try_new_with_control(control.finish(elements, memory)?, control)?
                .ok_or_else(array_shape_error)?
        }
        Value::Str(text) => parse_pg_array_literal_with_control(text, control)?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to {}[]",
                column_type_name(element)
            )))
        }
    };
    let converted = array_elements(array.elements(), array_scalar_type(element), control)?;
    let mut bounds = ProductionVec::new(*control);
    bounds.reserve(array.lower_bounds().len())?;
    for bound in array.lower_bounds() {
        bounds.push_copy(*bound)?;
    }
    let array = ArrayValue::with_lower_bounds_with_control(converted, bounds.finish()?, control)?
        .ok_or_else(array_shape_error)?;
    let (array, memory) = array.into_parts();
    Ok(control.finish(Value::Array(array), memory)?)
}

fn array_elements(
    values: &[Value],
    element: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    for value in values {
        let value = match value {
            Value::List(values) => {
                let (values, memory) = array_elements(values, element, control)?.into_parts();
                control.finish(Value::List(values), memory)?
            }
            scalar => convert_value_to_column_type_with_control(
                control.copy_value(scalar)?,
                element,
                control,
            )?,
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}

fn array_shape_error() -> SQLError {
    SQLError::TypeMismatch("multidimensional arrays must have matching dimensions".into())
}

fn record_value(
    value: Produced<Value>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let Value::Row(source) = &*value else {
        unreachable!();
    };
    let mut records = ProductionVec::new(*control);
    records.reserve(source.len())?;
    for index in 0..source.len() {
        let (name, memory) = control.format(format_args!("f{}", index + 1))?.into_parts();
        records.push_produced(control.finish((name, Value::Null), memory)?)?;
    }
    let (records, records_memory) = records.finish()?.into_parts();
    let (Value::Row(source), source_memory) = value.into_parts() else {
        unreachable!();
    };
    let old_buffer_bytes = source.capacity() * size_of::<Value>();
    let mut parts = RecordParts {
        source,
        records,
        memory: control.combine(source_memory, records_memory),
    };
    for ((_, destination), source) in parts.records.iter_mut().zip(parts.source) {
        control.check()?;
        *destination = source;
    }
    if let Some(memory) = &mut parts.memory {
        drop(memory.split(old_buffer_bytes));
    }
    Ok(control.finish(Value::Record(parts.records), parts.memory)?)
}

struct RecordParts {
    source: Vec<Value>,
    records: Vec<(String, Value)>,
    memory: Option<MemoryReservation>,
}

fn vector_value(vector: &[f32], control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(vector.len())?;
    for value in vector {
        output.push_produced(
            control.finish(Value::Float(f64::from(*value)), control.empty_reservation())?,
        )?;
    }
    let (values, memory) = output.finish()?.into_parts();
    Ok(control.finish(Value::List(values), memory)?)
}

fn parse_boolean_text(text: &str) -> Option<bool> {
    let text = text.trim();
    if ["true", "t", "yes", "y", "on", "1"]
        .iter()
        .any(|candidate| text.eq_ignore_ascii_case(candidate))
    {
        Some(true)
    } else if ["false", "f", "no", "n", "off", "0"]
        .iter()
        .any(|candidate| text.eq_ignore_ascii_case(candidate))
    {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
