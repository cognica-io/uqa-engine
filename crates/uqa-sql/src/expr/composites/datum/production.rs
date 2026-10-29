//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-width reinterpretation admits only the allocating enum and character carriers.

use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    DatumValue, Value,
};

pub(crate) fn reinterpret_with_control(
    value: &Value,
    before: &ColumnType,
    after: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    control.check()?;
    let before = super::base(before);
    let after = super::base(after);
    if before == after || super::pg_type_len(before) != super::pg_type_len(after) {
        return Ok(None);
    }
    let Some(bits) = super::encode_bits(value, before) else {
        return Ok(None);
    };
    decode_bits_with_control(bits, after, control)
}

pub(crate) fn decode_bits_with_control(
    bits: u64,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    control.check()?;
    match super::base(ty) {
        ColumnType::Enum(reference) => {
            let mut bytes = ProductionVec::new(*control);
            bytes.extend_from_slice(&(bits as u32).to_le_bytes())?;
            let (datum, memory) =
                DatumValue::with_control(reference.oid, 0, bytes.finish()?, control)?.into_parts();
            Ok(Some(control.finish(Value::Datum(datum), memory)?))
        }
        ColumnType::InternalChar => {
            let mut encoded = [0; 4];
            let text = if bits as u8 == 0 {
                ""
            } else {
                char::from(bits as u8).encode_utf8(&mut encoded)
            };
            let (text, memory) = control.copy_text(text)?.into_parts();
            Ok(Some(control.finish(Value::Str(text), memory)?))
        }
        _ => super::decode_inline_bits(bits, ty)
            .map(|value| {
                control
                    .finish(value, control.empty_reservation())
                    .map_err(Into::into)
            })
            .transpose(),
    }
}
