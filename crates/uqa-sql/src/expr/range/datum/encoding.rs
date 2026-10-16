//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range physical producers admit output and endpoint scratch under one original allowance.

use super::{
    align, corrupt, CanonicalMultirange, CanonicalRange, ColumnType, Produced, ProductionControl,
    ProductionVec, SQLError, Value, ABSOLUTE_OFFSET, EMPTY, LOWER_INCLUSIVE, LOWER_INFINITE,
    UPPER_INCLUSIVE, UPPER_INFINITE,
};
use crate::catalog::type_metadata::{pg_type_len, pg_type_oid};

/// The surrounding tuple or catalog constant owns the outer varlena header.
pub(crate) fn encode(text: &str, ty: &ColumnType) -> Result<Vec<u8>, SQLError> {
    encode_with_control(text, ty, &ProductionControl::uncontrolled()).map(|bytes| {
        bytes
            .into_uncontrolled()
            .expect("ordinary range Datum encoding")
    })
}

pub(super) fn encode_with_control(
    text: &str,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    control.check()?;
    let mut bytes = ProductionVec::new(*control);
    append(&mut bytes, &(pg_type_oid(ty) as u32).to_le_bytes())?;
    match ty {
        ColumnType::Range(subtype) => {
            let range = super::super::parse_range_with_control(text, *subtype, control)?;
            write_bounds(&range, &mut bytes, control)?;
            bytes.push_copy(flags(&range))?;
        }
        ColumnType::Multirange(subtype) => {
            let ranges = super::super::parse_multirange_with_control(text, *subtype, control)?;
            write_multirange(&ranges, &mut bytes, control)?;
        }
        _ => return Err(corrupt("non-range payload type")),
    }
    Ok(bytes.finish()?)
}

fn write_multirange(
    ranges: &CanonicalMultirange,
    bytes: &mut ProductionVec<'_, u8>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let count = ranges.ranges.len();
    let count_word = u32::try_from(count).map_err(|_| corrupt("too many ranges"))?;
    append(bytes, &count_word.to_le_bytes())?;
    let flags_start = 8 + count.saturating_sub(1) * 4;
    let start = align(flags_start + count + 4, &ranges.subtype.scalar_type())? - 4;
    pad_to(bytes, start)?;
    let mut previous = 0;
    for (index, range) in ranges.ranges.iter().enumerate() {
        control.check()?;
        let offset = u32::try_from(bytes.len() - start)
            .ok()
            .filter(|offset| *offset < ABSOLUTE_OFFSET)
            .ok_or_else(|| corrupt("range payload too large"))?;
        if index > 0 {
            let item = if index % 4 == 0 {
                offset | ABSOLUTE_OFFSET
            } else {
                offset - previous
            };
            bytes.as_mut_slice()[8 + (index - 1) * 4..8 + index * 4]
                .copy_from_slice(&item.to_le_bytes());
        }
        previous = offset;
        bytes.as_mut_slice()[flags_start + index] = flags(range);
        write_bounds(range, bytes, control)?;
        pad_to(
            bytes,
            align(bytes.len() + 4, &ranges.subtype.scalar_type())? - 4,
        )?;
    }
    Ok(())
}

fn flags(range: &CanonicalRange) -> u8 {
    if range.empty {
        return EMPTY;
    }
    let lower = if range.lower.is_none() {
        LOWER_INFINITE
    } else if range.lower_inclusive {
        LOWER_INCLUSIVE
    } else {
        0
    };
    let upper = if range.upper.is_none() {
        UPPER_INFINITE
    } else if range.upper_inclusive {
        UPPER_INCLUSIVE
    } else {
        0
    };
    lower | upper
}

fn write_bounds(
    range: &CanonicalRange,
    bytes: &mut ProductionVec<'_, u8>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let ty = range.subtype.scalar_type();
    for value in [range.lower.as_ref(), range.upper.as_ref()]
        .into_iter()
        .flatten()
    {
        control.check()?;
        if let Value::Decimal(number) = value {
            let payload =
                crate::catalog::node_tree::encode_numeric_datum_with_control(number, control)?;
            if payload.len() < 127 {
                bytes.push_copy(((payload.len() as u8 + 1) << 1) | 1)?;
            } else {
                pad_to(bytes, align(bytes.len() + 4, &ty)? - 4)?;
                let size = u32::try_from(payload.len() + 4)
                    .ok()
                    .and_then(|size| size.checked_mul(4))
                    .ok_or_else(|| corrupt("numeric bound too large"))?;
                append(bytes, &size.to_le_bytes())?;
            }
            append(bytes, &payload)?;
        } else {
            let bits = crate::expr::composites::datum::encode_bits(value, &ty)
                .ok_or_else(|| corrupt("invalid native range bound"))?;
            append(bytes, &bits.to_le_bytes()[..pg_type_len(&ty) as usize])?;
        }
    }
    Ok(())
}

fn append(bytes: &mut ProductionVec<'_, u8>, source: &[u8]) -> Result<(), SQLError> {
    bytes.reserve(source.len())?;
    for byte in source {
        bytes.push_copy(*byte)?;
    }
    Ok(())
}

fn pad_to(bytes: &mut ProductionVec<'_, u8>, length: usize) -> Result<(), SQLError> {
    bytes.reserve(length.saturating_sub(bytes.len()))?;
    while bytes.len() < length {
        bytes.push_copy(0)?;
    }
    Ok(())
}
