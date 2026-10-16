//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array elements and range endpoints share physical decoding without invoking scalar input or domain constraints.

use super::{corrupt, fixed, read_payload, word};
use crate::{
    catalog::type_metadata::{pg_type_align, pg_type_len, pg_type_oid},
    expr::SQLValueCatalog,
    ColumnType, SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    DatumValue, Value,
};

/// `origin` is the offset of `bytes[0]` from the containing Datum's alignment base. Domains retain their physical base representation; enum label observation remains deferred.
pub(in crate::expr) fn read(
    bytes: &[u8],
    offset: &mut usize,
    origin: usize,
    mut ty: &ColumnType,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    let length = pg_type_len(ty);
    let packed = length == -1 && bytes.get(*offset).is_some_and(|first| first & 1 != 0);
    if !packed {
        *offset = align(
            offset
                .checked_add(origin)
                .ok_or_else(|| corrupt("invalid inline datum offset"))?,
            pg_type_align(ty).as_bytes()[0],
        )? - origin;
    }
    let remaining = bytes
        .get(*offset..)
        .ok_or_else(|| corrupt("invalid inline datum length"))?;
    if length == -1 {
        let first = *remaining
            .first()
            .ok_or_else(|| corrupt("invalid inline datum length"))?;
        let (header, length) = if packed && first != 1 {
            (1, usize::from(first >> 1))
        } else {
            let header = word(remaining)
                .filter(|header| header.trailing_zeros() >= 2)
                .ok_or_else(|| corrupt("invalid inline datum header"))?;
            (4, (header >> 2) as usize)
        };
        let payload = remaining
            .get(header..length)
            .ok_or_else(|| corrupt("invalid inline datum length"))?;
        let value = read_payload(pg_type_oid(ty) as u32, payload, catalog, control)?;
        *offset += length;
        return Ok(value);
    }
    let length = usize::try_from(length).map_err(|_| corrupt("invalid inline datum width"))?;
    let source = remaining
        .get(..length)
        .ok_or_else(|| corrupt("invalid inline datum length"))?;
    let value = if let ColumnType::Enum(reference) = ty {
        let mut bytes = ProductionVec::new(*control);
        bytes.reserve(source.len())?;
        for byte in source {
            bytes.push_copy(*byte)?;
        }
        let (datum, memory) =
            DatumValue::with_control(reference.oid, 0, bytes.finish()?, control)?.into_parts();
        control.finish(Value::Datum(datum), memory)?
    } else {
        fixed::read_bytes(pg_type_oid(ty) as u32, source, control)?
    };
    *offset += length;
    Ok(value)
}

pub(super) fn align(offset: usize, alignment: u8) -> Result<usize, SQLError> {
    let mask = match alignment {
        b'c' => 0,
        b's' => 1,
        b'i' => 3,
        b'd' => 7,
        _ => return Err(corrupt("invalid datum alignment")),
    };
    offset
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or_else(|| corrupt("invalid datum length"))
}

#[cfg(test)]
mod tests;
