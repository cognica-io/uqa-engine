//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! By-reference fixed-width outputs borrow the retained tuple and reuse their SQL codecs.

use super::{corrupt, DatumValue, Produced, ProductionControl, SQLError, Value};

pub(super) fn read(
    datum: &DatumValue,
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>, SQLError>> {
    matches!(datum.type_oid(), 19 | 1186 | 2950).then(|| decode(datum, control))
}

fn decode(
    datum: &DatumValue,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let bytes = datum
        .bytes()
        .get(datum.offset() as usize..)
        .ok_or_else(|| corrupt("invalid datum length"))?;
    if datum.type_oid() == 19 {
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| corrupt("invalid name datum length"))?;
        let text = std::str::from_utf8(&bytes[..end])
            .map_err(|_| corrupt("invalid name datum encoding"))?;
        let (text, memory) = control.copy_text(text)?.into_parts();
        return Ok(control.finish(Value::Str(text), memory)?);
    }
    let bytes: [u8; 16] = bytes
        .get(..16)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| corrupt("invalid datum length"))?;
    if datum.type_oid() == 2950 {
        let (text, memory) =
            crate::expr::uuid::format_uuid_with_control(bytes, control)?.into_parts();
        Ok(control.finish(Value::Str(text), memory)?)
    } else {
        let value = crate::catalog::node_tree::decode_temporal_datum(&bytes, 1186)?;
        Ok(control.finish(Value::Temporal(value), None)?)
    }
}
