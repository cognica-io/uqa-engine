//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` native range payloads shared by catalog constants and retained tuple fields.

use super::{CanonicalRange, RangeSubtype};
use crate::catalog::type_metadata::{pg_type_align, pg_type_len, pg_type_oid};
use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    DecimalValue, Value,
};

const EMPTY: u8 = 1;
const LOWER_INCLUSIVE: u8 = 2;
const UPPER_INCLUSIVE: u8 = 4;
const LOWER_INFINITE: u8 = 8;
const UPPER_INFINITE: u8 = 16;
const ABSOLUTE_OFFSET: u32 = 1 << 31;

/// Encode an admitted native text carrier; the surrounding tuple or constant owns the varlena header.
pub(crate) fn encode(text: &str, ty: &ColumnType) -> Result<Vec<u8>, SQLError> {
    let mut bytes = (pg_type_oid(ty) as u32).to_le_bytes().to_vec();
    match ty {
        ColumnType::Range(subtype) => {
            let range = super::parse_range(text, *subtype)?;
            write_bounds(&range, &mut bytes)?;
            bytes.push(flags(&range));
        }
        ColumnType::Multirange(subtype) => {
            let ranges = super::parse_multirange(text, *subtype)?;
            let count = ranges.ranges.len();
            let count_word = u32::try_from(count).map_err(|_| corrupt("too many ranges"))?;
            bytes.extend_from_slice(&count_word.to_le_bytes());
            let flags_start = 8 + count.saturating_sub(1) * 4;
            let start = align(flags_start + count + 4, *subtype)? - 4;
            bytes.resize(start, 0);
            let mut previous = 0;
            for (index, range) in ranges.ranges.iter().enumerate() {
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
                    bytes[8 + (index - 1) * 4..8 + index * 4].copy_from_slice(&item.to_le_bytes());
                }
                previous = offset;
                bytes[flags_start + index] = flags(range);
                write_bounds(range, &mut bytes)?;
                bytes.resize(align(bytes.len() + 4, *subtype)? - 4, 0);
            }
        }
        _ => return Err(corrupt("non-range payload type")),
    }
    Ok(bytes)
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

fn write_bounds(range: &CanonicalRange, bytes: &mut Vec<u8>) -> Result<(), SQLError> {
    let ty = range.subtype.scalar_type();
    for value in [range.lower.as_ref(), range.upper.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Value::Decimal(number) = value {
            let payload = crate::catalog::node_tree::encode_numeric_datum(number)?;
            if payload.len() < 127 {
                bytes.push(((payload.len() as u8 + 1) << 1) | 1);
            } else {
                bytes.resize(align(bytes.len() + 4, range.subtype)? - 4, 0);
                let size = u32::try_from(payload.len() + 4)
                    .ok()
                    .and_then(|size| size.checked_mul(4))
                    .ok_or_else(|| corrupt("numeric bound too large"))?;
                bytes.extend_from_slice(&size.to_le_bytes());
            }
            bytes.extend_from_slice(&payload);
        } else {
            let bits = crate::expr::composites::datum::encode_bits(value, &ty)
                .ok_or_else(|| corrupt("invalid native range bound"))?;
            bytes.extend_from_slice(&bits.to_le_bytes()[..pg_type_len(&ty) as usize]);
        }
    }
    Ok(())
}

/// Observe already admitted bytes without rerunning input, canonicalization, or domain checks. Multirange members stream into the original output allowance.
pub(crate) fn read(
    bytes: &[u8],
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    if word(bytes)? != pg_type_oid(ty) as u32 {
        return Err(corrupt("range payload type differs from its declaration"));
    }
    let mut output = ProductionString::new(*control);
    match ty {
        ColumnType::Range(subtype) => {
            let flags = *bytes
                .last()
                .filter(|_| bytes.len() >= 5)
                .ok_or_else(|| corrupt("truncated range payload"))?;
            let bounds = &bytes[4..bytes.len() - 1];
            let (range, consumed) = read_bounds(bounds, flags, *subtype, control)?;
            if consumed != bounds.len() {
                return Err(corrupt("invalid range bound length"));
            }
            super::production::write_range(&range, &mut output, control)?;
        }
        ColumnType::Multirange(subtype) => {
            read_multirange(bytes, *subtype, &mut output, control)?;
        }
        _ => return Err(corrupt("non-range payload type")),
    }
    let (text, memory) = output.finish()?.into_parts();
    Ok(control.finish(Value::Str(text), memory)?)
}

fn read_multirange(
    bytes: &[u8],
    subtype: RangeSubtype,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let count = word(bytes.get(4..).unwrap_or_default())? as usize;
    let flags_start = count
        .saturating_sub(1)
        .checked_mul(4)
        .and_then(|size| size.checked_add(8))
        .ok_or_else(|| corrupt("invalid multirange count"))?;
    let flags_end = flags_start
        .checked_add(count)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| corrupt("invalid multirange count"))?;
    let start = align(flags_end + 4, subtype)? - 4;
    let bounds = bytes
        .get(start..)
        .ok_or_else(|| corrupt("truncated multirange payload"))?;
    output.push('{')?;
    let mut offset = 0;
    let mut previous = 0_usize;
    for index in 0..count {
        control.check()?;
        if index > 0 {
            let item = word(&bytes[8 + (index - 1) * 4..])?;
            let recorded = if item & ABSOLUTE_OFFSET != 0 {
                (item & !ABSOLUTE_OFFSET) as usize
            } else {
                previous
                    .checked_add(item as usize)
                    .ok_or_else(|| corrupt("invalid multirange offset"))?
            };
            if recorded != offset {
                return Err(corrupt("invalid multirange offset"));
            }
            output.push(',')?;
        }
        previous = offset;
        let flag = bytes[flags_start + index];
        if flag & EMPTY != 0 {
            return Err(corrupt("empty multirange member"));
        }
        let remaining = bounds
            .get(offset..)
            .ok_or_else(|| corrupt("invalid multirange offset"))?;
        let (range, consumed) = read_bounds(remaining, flag, subtype, control)?;
        super::production::write_range(&range, output, control)?;
        offset = offset
            .checked_add(align(consumed, subtype)?)
            .ok_or_else(|| corrupt("invalid multirange length"))?;
    }
    if offset != bounds.len() {
        return Err(corrupt("invalid multirange bound length"));
    }
    output.push('}')?;
    Ok(())
}

fn read_bounds(
    bytes: &[u8],
    flags: u8,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<(Produced<CanonicalRange>, usize), SQLError> {
    if flags & !31 != 0
        || (flags & EMPTY != 0 && flags != EMPTY)
        || flags & (LOWER_INFINITE | LOWER_INCLUSIVE) == LOWER_INFINITE | LOWER_INCLUSIVE
        || flags & (UPPER_INFINITE | UPPER_INCLUSIVE) == UPPER_INFINITE | UPPER_INCLUSIVE
    {
        return Err(corrupt("invalid range flags"));
    }
    if flags == EMPTY {
        return Ok((
            control.finish(CanonicalRange::empty(subtype), control.empty_reservation())?,
            0,
        ));
    }
    let mut offset = 0;
    let lower = read_bound(
        bytes,
        &mut offset,
        flags & LOWER_INFINITE != 0,
        subtype,
        control,
    )?;
    let upper = read_bound(
        bytes,
        &mut offset,
        flags & UPPER_INFINITE != 0,
        subtype,
        control,
    )?;
    let (lower, lower_memory) = lower.into_parts();
    let (upper, upper_memory) = upper.into_parts();
    let range = CanonicalRange {
        subtype,
        lower,
        upper,
        lower_inclusive: flags & LOWER_INCLUSIVE != 0,
        upper_inclusive: flags & UPPER_INCLUSIVE != 0,
        empty: false,
    };
    Ok((
        control.finish(range, control.combine(lower_memory, upper_memory))?,
        offset,
    ))
}

fn read_bound(
    bytes: &[u8],
    offset: &mut usize,
    infinite: bool,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Value>>, SQLError> {
    control.check()?;
    if infinite {
        return Ok(control.finish(None, control.empty_reservation())?);
    }
    let ty = subtype.scalar_type();
    let value = if subtype == RangeSubtype::Numeric {
        let first = *bytes
            .get(*offset)
            .ok_or_else(|| corrupt("truncated numeric bound"))?;
        let (header, size) = if first & 1 != 0 {
            (1, usize::from(first >> 1))
        } else {
            *offset = align(*offset, subtype)?;
            let header = word(bytes.get(*offset..).unwrap_or_default())?;
            if header & 3 != 0 {
                return Err(corrupt("invalid numeric bound header"));
            }
            (4, (header >> 2) as usize)
        };
        let end = offset
            .checked_add(size)
            .ok_or_else(|| corrupt("invalid numeric bound length"))?;
        let payload = bytes
            .get(*offset + header..end)
            .filter(|_| size >= header)
            .ok_or_else(|| corrupt("invalid numeric bound length"))?;
        let text = crate::catalog::node_tree::decode_numeric_datum_with_control(payload, control)?;
        let (number, memory) = DecimalValue::parse_with_control(&text, control)?
            .ok_or_else(|| corrupt("invalid numeric bound"))?
            .into_parts();
        *offset = end;
        control.finish(Value::Decimal(number), memory)?
    } else {
        let size = pg_type_len(&ty) as usize;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| corrupt("invalid range bound length"))?;
        let source = bytes
            .get(*offset..end)
            .ok_or_else(|| corrupt("truncated range bound"))?;
        let mut bits = [0; 8];
        bits[..size].copy_from_slice(source);
        let value = crate::expr::composites::datum::decode_bits(u64::from_le_bytes(bits), &ty)
            .ok_or_else(|| corrupt("invalid range bound"))?;
        *offset = end;
        control.finish(value, control.empty_reservation())?
    };
    let (value, memory) = value.into_parts();
    Ok(control.finish(Some(value), memory)?)
}

fn align(offset: usize, subtype: RangeSubtype) -> Result<usize, SQLError> {
    let mask = if pg_type_align(&subtype.scalar_type()) == "d" {
        7
    } else {
        3
    };
    offset
        .checked_add(mask)
        .map(|offset| offset & !mask)
        .ok_or_else(|| corrupt("invalid range payload length"))
}

fn word(bytes: &[u8]) -> Result<u32, SQLError> {
    bytes
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| corrupt("truncated range payload"))
}

fn corrupt(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
