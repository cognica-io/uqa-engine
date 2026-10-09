//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-width outputs borrow the retained tuple and reuse their SQL scalar codecs.

use super::{corrupt, DatumValue, Produced, ProductionControl, SQLError, Value};
use crate::catalog::type_metadata::{builtin_scalar_type, pg_type_by_value, pg_type_len};

pub(super) fn read(
    datum: &DatumValue,
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>, SQLError>> {
    (matches!(datum.type_oid(), 19 | 1186 | 1266 | 2950)
        || builtin_scalar_type(datum.type_oid()).is_some_and(pg_type_by_value))
    .then(|| decode(datum, control))
}

fn decode(
    datum: &DatumValue,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let bytes = datum
        .bytes()
        .get(datum.offset() as usize..)
        .ok_or_else(|| corrupt("invalid datum length"))?;
    read_bytes(datum.type_oid(), bytes, control)
}

pub(super) fn read_bytes(
    oid: u32,
    bytes: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    if let Some(ty) = builtin_scalar_type(oid).filter(|ty| pg_type_by_value(ty)) {
        let length = usize::try_from(pg_type_len(ty))
            .ok()
            .filter(|length| *length <= 8)
            .ok_or_else(|| corrupt("invalid fixed datum width"))?;
        let bytes = bytes
            .get(..length)
            .ok_or_else(|| corrupt("invalid datum length"))?;
        let mut bits = [0; 8];
        bits[..length].copy_from_slice(bytes);
        let value = crate::expr::composites::datum::decode_bits(u64::from_le_bytes(bits), ty)
            .ok_or_else(|| {
                SQLError::Unsupported(format!("physical fixed datum output for type OID {oid}"))
            })?;
        return Ok(control.copy_value(&value)?);
    }
    if oid == 19 {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| corrupt("invalid name datum length"))?;
        let text = std::str::from_utf8(&bytes[..end])
            .map_err(|_| corrupt("invalid name datum encoding"))?;
        let (text, memory) = control.copy_text(text)?.into_parts();
        return Ok(control.finish(Value::Str(text), memory)?);
    }
    if matches!(oid, 1186 | 1266) {
        let length = if oid == 1186 { 16 } else { 12 };
        let bytes = bytes
            .get(..length)
            .ok_or_else(|| corrupt("invalid datum length"))?;
        let value = crate::catalog::node_tree::decode_temporal_datum(bytes, i64::from(oid))?;
        return Ok(control.finish(Value::Temporal(value), control.empty_reservation())?);
    }
    let bytes: [u8; 16] = bytes
        .get(..16)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| corrupt("invalid datum length"))?;
    if oid == 2950 {
        let (text, memory) =
            crate::expr::uuid::format_uuid_with_control(bytes, control)?.into_parts();
        Ok(control.finish(Value::Str(text), memory)?)
    } else {
        Err(SQLError::Unsupported(format!(
            "physical fixed datum output for type OID {oid}"
        )))
    }
}
