//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read retained physical scalar bytes only when a SQL operation observes the value. Type inspection, copying and NULL tests do not invoke this decoder.

use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    DatumValue, DecimalValue, Value,
};

mod arrays;
mod compression;
mod fixed;
mod malformed;

#[cfg(test)]
mod tests;

pub fn type_name(datum: &DatumValue) -> &'static str {
    match datum.type_oid() {
        18 => "\"char\"",
        19 => "name",
        700 => "real",
        1042 => "character",
        1043 => "character varying",
        oid => crate::catalog::type_metadata::catalog_type_name(i64::from(oid)),
    }
}

pub fn read(datum: &DatumValue) -> Result<Value, SQLError> {
    read_with_catalog(datum, None)
}

/// Read an admitted physical value through the current statement's catalog, without invoking type input or domain checks.
pub fn read_with_catalog(
    datum: &DatumValue,
    engine: Option<&dyn super::EngineHook>,
) -> Result<Value, SQLError> {
    read_with_catalog_and_control(datum, engine, &ProductionControl::uncontrolled())
        .map(|value| value.into_uncontrolled().expect("ordinary datum read"))
}

/// Copy a selected constant field at planning time, detoasting variable-width storage without interpreting its scalar contents. Whole records remain deferred.
pub fn copy_constant_field(value: &Value, ty: &crate::ColumnType) -> Result<Value, SQLError> {
    let Value::Datum(datum) = value else {
        return Ok(value.clone());
    };
    if crate::catalog::type_metadata::pg_type_len(ty) != -1 {
        return Ok(value.clone());
    }
    let control = ProductionControl::uncontrolled();
    let payload = payload(datum, &control)?;
    let size = payload
        .bytes()
        .len()
        .checked_add(4)
        .and_then(|size| u32::try_from(size).ok())
        .and_then(|size| size.checked_mul(4))
        .ok_or_else(|| corrupt("invalid datum length"))?;
    let mut bytes = size.to_le_bytes().to_vec();
    bytes.extend_from_slice(payload.bytes());
    Ok(Value::Datum(DatumValue::new(datum.type_oid(), 0, bytes)))
}

pub fn read_with_control(
    datum: &DatumValue,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    read_with_catalog_and_control(datum, None, control)
}

/// Preserve the caller's output allowance and cancellation boundary during catalog-dependent physical reads.
pub fn read_with_catalog_and_control(
    datum: &DatumValue,
    engine: Option<&dyn super::EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    read_with_enum_catalog_and_control(
        datum,
        engine.and_then(super::EngineHook::enum_labels),
        control,
    )
}

pub(crate) fn read_with_enum_catalog_and_control(
    datum: &DatumValue,
    enums: Option<&dyn super::enums::EnumLabelCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    if let Some(value) = fixed::read(datum, control) {
        return value;
    }
    if crate::catalog::type_metadata::builtin_scalar_type(datum.type_oid()).is_none()
        && crate::catalog::type_metadata::builtin_array_element(datum.type_oid()).is_none()
    {
        if let Some(enums) = enums {
            if enums.enum_type_labels(datum.type_oid())?.is_some() {
                let oid = word(
                    datum
                        .bytes()
                        .get(datum.offset() as usize..)
                        .unwrap_or_default(),
                )
                .ok_or_else(|| corrupt("invalid datum length"))?;
                return Ok(
                    control.retain_external_value(super::enums::enum_value_from_oid(
                        Some(enums),
                        oid,
                    )?)?,
                );
            }
        }
    }
    let payload = payload(datum, control)?;
    read_payload(datum.type_oid(), payload.bytes(), control)
}

pub(super) fn compare_jsonb_with_control(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<std::cmp::Ordering>, SQLError> {
    let physical = |value: &Value| matches!(value, Value::Datum(datum) if datum.type_oid() == 3802);
    let jsonb = |value: &Value| physical(value) || matches!(value, Value::JsonB(_));
    if !(jsonb(left) && jsonb(right) && (physical(left) || physical(right))) {
        return Ok(None);
    }
    let left_bytes = match left {
        Value::Datum(datum) => Some(payload(datum, control)?),
        _ => None,
    };
    let right_bytes = match right {
        Value::Datum(datum) => Some(payload(datum, control)?),
        _ => None,
    };
    let left = match left {
        Value::JsonB(text) => super::json::JsonbInput::Text(text),
        _ => super::json::JsonbInput::Bytes(left_bytes.as_ref().expect("physical JSONB").bytes()),
    };
    let right = match right {
        Value::JsonB(text) => super::json::JsonbInput::Text(text),
        _ => super::json::JsonbInput::Bytes(right_bytes.as_ref().expect("physical JSONB").bytes()),
    };
    super::json::compare_jsonb_datums_with_control(left, right, control).map(Some)
}

fn read_payload(
    oid: u32,
    bytes: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    if crate::catalog::type_metadata::builtin_array_element(oid).is_some() {
        return arrays::read(bytes, control);
    }
    match oid {
        17 => {
            let mut output = ProductionVec::new(*control);
            output.reserve(bytes.len())?;
            for byte in bytes {
                output.push_copy(*byte)?;
            }
            let (bytes, memory) = output.finish()?.into_parts();
            Ok(control.finish(Value::Bytes(bytes), memory)?)
        }
        25 | 114 | 194 | 1042 | 1043 | 1790 => {
            let text = std::str::from_utf8(bytes).map_err(|error| SQLError::Routine {
                sqlstate: "22021".into(),
                message: format!(
                    "invalid byte sequence for encoding \"UTF8\": 0x{:02x}",
                    bytes[error.valid_up_to()]
                ),
            })?;
            let (text, memory) = control.copy_text(text)?.into_parts();
            let value = match oid {
                114 => Value::Json(text),
                1042 => Value::FixedChar(text),
                _ => Value::Str(text),
            };
            Ok(control.finish(value, memory)?)
        }
        3802 => {
            let (text, memory) =
                super::json::decode_jsonb_datum_with_control(bytes, control)?.into_parts();
            Ok(control.finish(Value::JsonB(text), memory)?)
        }
        1700 => {
            let text =
                crate::catalog::node_tree::decode_numeric_datum_with_control(bytes, control)?;
            let (number, memory) = DecimalValue::parse_with_control(&text, control)?
                .ok_or_else(|| corrupt("invalid numeric datum"))?
                .into_parts();
            Ok(control.finish(Value::Decimal(number), memory)?)
        }
        oid => Err(SQLError::Unsupported(format!(
            "physical datum output for type OID {oid}"
        ))),
    }
}

/// Borrow ordinary and inline physical bytea payloads; compressed input retains its admitted decompression buffer through consumption.
pub(super) fn binary_payload<'a>(
    value: &'a Value,
    control: &ProductionControl<'_>,
) -> Result<Option<Payload<'a>>, SQLError> {
    control.check()?;
    match value {
        Value::Bytes(bytes) => Ok(Some(Payload::Borrowed(bytes))),
        Value::Datum(datum) if datum.type_oid() == 17 => payload(datum, control).map(Some),
        _ => Ok(None),
    }
}

/// Binary length observes the raw-size header and does not decompress the payload.
pub(super) fn binary_length(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<i64>, SQLError> {
    control.check()?;
    let datum = match value {
        Value::Bytes(bytes) => return Ok(Some(bytes.len() as i64)),
        Value::Datum(datum) if datum.type_oid() == 17 => datum,
        _ => return Ok(None),
    };
    let bytes = datum
        .bytes()
        .get(datum.offset() as usize..)
        .ok_or_else(|| corrupt("invalid datum length"))?;
    let first = *bytes
        .first()
        .ok_or_else(|| corrupt("invalid datum length"))?;
    let size = if first == 1 && bytes.get(1) == Some(&18) {
        i64::from(
            word(bytes.get(2..).unwrap_or_default())
                .ok_or_else(|| corrupt("invalid datum length"))?,
        ) - 4
    } else if first & 1 != 0 {
        i64::from(first >> 1) - 1
    } else if first & 3 == 2 {
        i64::from(
            word(bytes.get(4..).unwrap_or_default())
                .ok_or_else(|| corrupt("invalid datum length"))?
                & 0x3fff_ffff,
        )
    } else {
        i64::from(word(bytes).ok_or_else(|| corrupt("invalid datum length"))? >> 2) - 4
    };
    Ok(Some(size))
}

pub(super) enum Payload<'a> {
    Borrowed(&'a [u8]),
    Owned(Produced<Vec<u8>>),
}

impl Payload<'_> {
    pub(super) fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }
}

fn payload<'a>(
    datum: &'a DatumValue,
    control: &ProductionControl<'_>,
) -> Result<Payload<'a>, SQLError> {
    let offset = datum.offset() as usize;
    let bytes = datum
        .bytes()
        .get(offset..)
        .ok_or_else(|| corrupt("invalid datum length"))?;
    let first = *bytes
        .first()
        .ok_or_else(|| corrupt("invalid datum length"))?;
    if first == 1 {
        return Err(malformed::external(datum));
    }
    if first & 1 != 0 {
        let length = usize::from(first >> 1);
        return bytes
            .get(1..length)
            .map(Payload::Borrowed)
            .ok_or_else(|| corrupt("invalid datum length"));
    }
    let header = word(bytes).ok_or_else(|| corrupt("invalid datum length"))?;
    let length = (header >> 2) as usize;
    if header & 3 == 2 {
        let information = word(bytes.get(4..).unwrap_or_default()).unwrap_or(0);
        let method = information >> 30;
        let compressed = bytes
            .get(8..length)
            .ok_or_else(|| compression::corrupt(method))?;
        return compression::decompress(
            compressed,
            (information & 0x3fff_ffff) as usize,
            method,
            control,
        )
        .map(Payload::Owned);
    }
    bytes
        .get(4..length)
        .map(Payload::Borrowed)
        .ok_or_else(|| corrupt("invalid datum length"))
}

fn word(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?))
}

fn corrupt(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: message.into(),
    }
}
