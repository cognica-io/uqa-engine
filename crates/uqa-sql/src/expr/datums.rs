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

mod compression;
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
    read_with_control(datum, &ProductionControl::uncontrolled())
        .map(|value| value.into_uncontrolled().expect("ordinary datum read"))
}

pub fn read_with_control(
    datum: &DatumValue,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let payload = payload(datum, control)?;
    let bytes = payload.bytes();
    match datum.type_oid() {
        17 => {
            let mut output = ProductionVec::new(*control);
            output.reserve(bytes.len())?;
            for byte in bytes {
                output.push_copy(*byte)?;
            }
            let (bytes, memory) = output.finish()?.into_parts();
            Ok(control.finish(Value::Bytes(bytes), memory)?)
        }
        25 | 114 | 1042 | 1043 | 1790 => {
            let text = std::str::from_utf8(bytes).map_err(|error| SQLError::Routine {
                sqlstate: "22021".into(),
                message: format!(
                    "invalid byte sequence for encoding \"UTF8\": 0x{:02x}",
                    bytes[error.valid_up_to()]
                ),
            })?;
            let (text, memory) = control.copy_text(text)?.into_parts();
            let value = match datum.type_oid() {
                114 => Value::Json(text),
                1042 => Value::FixedChar(text),
                _ => Value::Str(text),
            };
            Ok(control.finish(value, memory)?)
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

enum Payload<'a> {
    Borrowed(&'a [u8]),
    Owned(Produced<Vec<u8>>),
}

impl Payload<'_> {
    fn bytes(&self) -> &[u8] {
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
