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
use crate::{
    catalog::type_metadata::{pg_type_len, pg_type_oid, pg_type_storage},
    expr::{
        composites::CompositeTypeCatalog,
        datums::encoding::{append, header, pad_to},
        range::RangeValueDescriptor,
    },
};

/// The surrounding tuple or catalog constant owns the outer varlena header.
pub(crate) fn encode(text: &str, ty: &ColumnType) -> Result<Vec<u8>, SQLError> {
    encode_with_control(text, ty, &ProductionControl::uncontrolled()).map(|bytes| {
        bytes
            .into_uncontrolled()
            .expect("ordinary range Datum encoding")
    })
}

pub(crate) fn encode_with_control(
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
            write_bounds(&range, &subtype.scalar_type(), None, &mut bytes, control)?;
            bytes.push_copy(flags(&range))?;
        }
        ColumnType::Multirange(subtype) => {
            let ranges = super::super::parse_multirange_with_control(text, *subtype, control)?;
            write_multirange(&ranges, &subtype.scalar_type(), None, &mut bytes, control)?;
        }
        _ => return Err(corrupt("non-range payload type")),
    }
    Ok(bytes.finish()?)
}

fn write_multirange<I>(
    ranges: &CanonicalMultirange<I>,
    endpoint: &ColumnType,
    catalog: Option<&dyn CompositeTypeCatalog>,
    bytes: &mut ProductionVec<'_, u8>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let count = ranges.ranges.len();
    let count_word = u32::try_from(count).map_err(|_| corrupt("too many ranges"))?;
    append(bytes, &count_word.to_le_bytes())?;
    let flags_start = 8 + count.saturating_sub(1) * 4;
    let start = align(flags_start + count + 4, endpoint)? - 4;
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
        write_bounds(range, endpoint, catalog, bytes, control)?;
        pad_to(bytes, align(bytes.len() + 4, endpoint)? - 4)?;
    }
    Ok(())
}

fn flags<I>(range: &CanonicalRange<I>) -> u8 {
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

fn write_bounds<I>(
    range: &CanonicalRange<I>,
    ty: &ColumnType,
    catalog: Option<&dyn CompositeTypeCatalog>,
    bytes: &mut ProductionVec<'_, u8>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for value in [range.lower.as_ref(), range.upper.as_ref()]
        .into_iter()
        .flatten()
    {
        control.check()?;
        let payload = crate::expr::datums::encoding::encode(value, ty, catalog, control)?;
        let length = pg_type_len(ty);
        if length == -1 {
            if payload.len() < 127 && pg_type_storage(ty) != "p" {
                bytes.push_copy(((payload.len() as u8 + 1) << 1) | 1)?;
            } else {
                pad_to(bytes, align(bytes.len() + 4, ty)? - 4)?;
                append(bytes, &header(payload.len())?)?;
            }
        } else {
            if usize::try_from(length).ok() != Some(payload.len()) {
                return Err(corrupt("invalid range bound width"));
            }
            pad_to(bytes, align(bytes.len() + 4, ty)? - 4)?;
        }
        append(bytes, &payload)?;
    }
    Ok(())
}

/// Preserve a completed range's identity and admitted bounds in the physical carrier, without repeating any selected construction operation.
pub fn declared_range_datum_with_control(
    range: &CanonicalRange<u32>,
    descriptor: &RangeValueDescriptor,
    catalog: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    check_identity(range.subtype, descriptor)?;
    let mut bytes = ProductionVec::new(*control);
    append(&mut bytes, &descriptor.range_oid.to_le_bytes())?;
    write_bounds(range, &descriptor.endpoint, catalog, &mut bytes, control)?;
    bytes.push_copy(flags(range))?;
    finish_datum(descriptor.range_oid, bytes, control)
}

/// The caller supplies an already normalized multirange; serialization neither sorts members nor invokes canonicalization.
pub fn declared_multirange_datum_with_control(
    ranges: &CanonicalMultirange<u32>,
    descriptor: &RangeValueDescriptor,
    catalog: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    check_identity(ranges.subtype, descriptor)?;
    for range in &ranges.ranges {
        control.check()?;
        check_identity(range.subtype, descriptor)?;
    }
    let mut bytes = ProductionVec::new(*control);
    append(&mut bytes, &descriptor.multirange_oid.to_le_bytes())?;
    write_multirange(ranges, &descriptor.endpoint, catalog, &mut bytes, control)?;
    finish_datum(descriptor.multirange_oid, bytes, control)
}

fn check_identity(oid: u32, descriptor: &RangeValueDescriptor) -> Result<(), SQLError> {
    if oid == 0
        || oid != descriptor.range_oid
        || descriptor.multirange_oid == 0
        || oid == descriptor.multirange_oid
    {
        return Err(corrupt("inconsistent range type descriptor"));
    }
    Ok(())
}

fn finish_datum(
    oid: u32,
    mut bytes: ProductionVec<'_, u8>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let length = bytes.len();
    let header = header(length)?;
    append(&mut bytes, &[0; 4])?;
    bytes.as_mut_slice().copy_within(..length, 4);
    bytes.as_mut_slice()[..4].copy_from_slice(&header);
    let (datum, memory) =
        uqa_core::DatumValue::with_control(oid, 0, bytes.finish()?, control)?.into_parts();
    Ok(control.finish(Value::Datum(datum), memory)?)
}
