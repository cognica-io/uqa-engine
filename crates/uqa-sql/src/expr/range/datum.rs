//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` native range payloads shared by catalog constants and retained tuple fields.

use super::{CanonicalMultirange, CanonicalRange, RangeComparisonValue, RangeSubtype};
use crate::catalog::type_metadata::{pg_type_align, pg_type_len, pg_type_oid};
use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
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
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let subtype = physical_subtype(bytes, ty, catalog)?;
    let mut output = ProductionString::new(*control);
    match ty {
        ColumnType::Range(_) => {
            let range = read_range(bytes, subtype, control)?;
            super::production::write_range(&range, &mut output, control)?;
        }
        ColumnType::Multirange(_) => {
            let mut ranges = Members::new(bytes, subtype)?;
            output.push('{')?;
            let mut separator = false;
            while let Some(range) = ranges.next(control)? {
                if separator {
                    output.push(',')?;
                }
                super::production::write_range(&range, &mut output, control)?;
                separator = true;
            }
            output.push('}')?;
        }
        _ => return Err(corrupt("non-range payload type")),
    }
    let (text, memory) = output.finish()?.into_parts();
    Ok(control.finish(Value::Str(text), memory)?)
}

/// Typed consumers keep the physical range identity and admitted endpoints instead of decoding to text and invoking input again.
pub(in crate::expr) fn comparison(
    bytes: &[u8],
    ty: &ColumnType,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<RangeComparisonValue, SQLError> {
    control.check()?;
    let subtype = physical_subtype(bytes, ty, catalog)?;
    match ty {
        ColumnType::Range(_) => Ok(RangeComparisonValue::Range(read_range(
            bytes, subtype, control,
        )?)),
        ColumnType::Multirange(_) => {
            let mut input = Members::new(bytes, subtype)?;
            let mut ranges = ProductionVec::new(*control);
            while let Some(range) = input.next(control)? {
                ranges.push_produced(range)?;
            }
            let (ranges, memory) = ranges.finish()?.into_parts();
            Ok(RangeComparisonValue::Multirange(
                control.finish(CanonicalMultirange { subtype, ranges }, memory)?,
            ))
        }
        _ => Err(corrupt("non-range payload type")),
    }
}

fn physical_subtype(
    bytes: &[u8],
    declared: &ColumnType,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
) -> Result<RangeSubtype, SQLError> {
    use crate::catalog::type_metadata::{
        builtin_array_element, builtin_scalar_type, routine_pseudo_type_name,
    };
    let oid = word(bytes)?;
    let actual = builtin_scalar_type(oid);
    match (declared, actual) {
        (ColumnType::Range(_), Some(ColumnType::Range(subtype)))
        | (ColumnType::Multirange(_), Some(ColumnType::Multirange(subtype))) => {
            return Ok(*subtype)
        }
        (ColumnType::Range(_) | ColumnType::Multirange(_), _) => {}
        _ => return Err(corrupt("non-range payload type")),
    }
    let exists = actual.is_some()
        || builtin_array_element(oid).is_some()
        || routine_pseudo_type_name(i64::from(oid)).is_some()
        || catalog
            .map(|catalog| catalog.value_type_exists(oid))
            .transpose()?
            .unwrap_or(false);
    if !exists {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type with OID {oid} does not exist"),
        });
    }
    Err(SQLError::Routine {
        sqlstate: "XX000".into(),
        message: format!(
            "type {oid} is not a {} type",
            if matches!(declared, ColumnType::Range(_)) {
                "range"
            } else {
                "multirange"
            }
        ),
    })
}

fn read_range(
    bytes: &[u8],
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalRange>, SQLError> {
    let flags = *bytes
        .last()
        .filter(|_| bytes.len() >= 5)
        .ok_or_else(|| corrupt("truncated range payload"))?;
    let bounds = &bytes[4..bytes.len() - 1];
    let (range, consumed) = read_bounds(bounds, flags, subtype, control)?;
    if consumed != bounds.len() {
        return Err(corrupt("invalid range bound length"));
    }
    Ok(range)
}

struct Members<'a> {
    bytes: &'a [u8],
    bounds: &'a [u8],
    flags: &'a [u8],
    subtype: RangeSubtype,
    index: usize,
    offset: usize,
    previous: usize,
}

impl<'a> Members<'a> {
    fn new(bytes: &'a [u8], subtype: RangeSubtype) -> Result<Self, SQLError> {
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
        Ok(Self {
            bytes,
            bounds,
            flags: &bytes[flags_start..flags_end],
            subtype,
            index: 0,
            offset: 0,
            previous: 0,
        })
    }

    fn next(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<CanonicalRange>>, SQLError> {
        control.check()?;
        if self.index == self.flags.len() {
            if self.offset != self.bounds.len() {
                return Err(corrupt("invalid multirange bound length"));
            }
            return Ok(None);
        }
        if self.index > 0 {
            let item = word(&self.bytes[8 + (self.index - 1) * 4..])?;
            let recorded = if item & ABSOLUTE_OFFSET != 0 {
                (item & !ABSOLUTE_OFFSET) as usize
            } else {
                self.previous
                    .checked_add(item as usize)
                    .ok_or_else(|| corrupt("invalid multirange offset"))?
            };
            if recorded != self.offset {
                return Err(corrupt("invalid multirange offset"));
            }
        }
        self.previous = self.offset;
        let flag = self.flags[self.index];
        if flag & EMPTY != 0 {
            return Err(corrupt("empty multirange member"));
        }
        let remaining = self
            .bounds
            .get(self.offset..)
            .ok_or_else(|| corrupt("invalid multirange offset"))?;
        let (range, consumed) = read_bounds(remaining, flag, self.subtype, control)?;
        self.offset = self
            .offset
            .checked_add(align(consumed, self.subtype)?)
            .ok_or_else(|| corrupt("invalid multirange length"))?;
        self.index += 1;
        Ok(Some(range))
    }
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
