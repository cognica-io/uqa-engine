//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` native range payloads shared by catalog constants and retained tuple fields.

use super::{CanonicalMultirange, CanonicalRange, RangeComparisonValue, RangeSubtype};
use crate::catalog::type_metadata::pg_type_align;
use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    Value,
};

const EMPTY: u8 = 1;
const LOWER_INCLUSIVE: u8 = 2;
const UPPER_INCLUSIVE: u8 = 4;
const LOWER_INFINITE: u8 = 8;
const UPPER_INFINITE: u8 = 16;
const ABSOLUTE_OFFSET: u32 = 1 << 31;

mod catalog;
mod encoding;
use catalog::PhysicalRangeType;
pub(crate) use encoding::encode;

/// Observe already admitted bytes without rerunning input, canonicalization, or domain checks. Multirange members stream into the original output allowance.
pub(crate) fn read(
    bytes: &[u8],
    ty: &ColumnType,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    read_class(bytes, catalog::is_multirange(ty)?, catalog, control)
}

pub(crate) fn read_class(
    bytes: &[u8],
    multirange: bool,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let selected = catalog::resolve_class(bytes, multirange, catalog)?;
    let native_endpoint;
    let (oid, endpoint) = match &selected {
        PhysicalRangeType::Native(subtype) => {
            native_endpoint = subtype.scalar_type();
            (
                crate::catalog::type_metadata::pg_type_oid(&ColumnType::Range(*subtype)) as u32,
                &native_endpoint,
            )
        }
        PhysicalRangeType::Declared(descriptor) => (descriptor.range_oid, &descriptor.endpoint),
    };
    let mut output = ProductionString::new(*control);
    let render = |value: &Value, control: &ProductionControl<'_>| {
        crate::result::format_postgres_text_with_catalog_and_control(
            value, endpoint, catalog, control,
        )
    };
    if multirange {
        let mut ranges = Members::new(bytes, oid, endpoint, catalog)?;
        output.push('{')?;
        let mut separator = false;
        while let Some(range) = ranges.next(control)? {
            if separator {
                output.push(',')?;
            }
            super::production::write_range_with(&range, &mut output, control, render)?;
            separator = true;
        }
        output.push('}')?;
    } else {
        let range = read_range(bytes, oid, endpoint, catalog, control)?;
        super::production::write_range_with(&range, &mut output, control, render)?;
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
    let multirange = catalog::is_multirange(ty)?;
    match catalog::resolve(bytes, ty, catalog)? {
        PhysicalRangeType::Native(subtype) => {
            let endpoint = subtype.scalar_type();
            if multirange {
                Ok(RangeComparisonValue::Multirange(read_members(
                    bytes, subtype, &endpoint, catalog, control,
                )?))
            } else {
                Ok(RangeComparisonValue::Range(read_range(
                    bytes, subtype, &endpoint, catalog, control,
                )?))
            }
        }
        PhysicalRangeType::Declared(descriptor) => {
            if multirange {
                Ok(RangeComparisonValue::DeclaredMultirange(read_members(
                    bytes,
                    descriptor.range_oid,
                    &descriptor.endpoint,
                    catalog,
                    control,
                )?))
            } else {
                Ok(RangeComparisonValue::DeclaredRange(read_range(
                    bytes,
                    descriptor.range_oid,
                    &descriptor.endpoint,
                    catalog,
                    control,
                )?))
            }
        }
    }
}

fn read_members<I: Copy>(
    bytes: &[u8],
    subtype: I,
    endpoint: &ColumnType,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalMultirange<I>>, SQLError> {
    let mut input = Members::new(bytes, subtype, endpoint, catalog)?;
    let mut ranges = ProductionVec::new(*control);
    while let Some(range) = input.next(control)? {
        ranges.push_produced(range)?;
    }
    let (ranges, memory) = ranges.finish()?.into_parts();
    Ok(control.finish(CanonicalMultirange { subtype, ranges }, memory)?)
}

fn read_range<I: Copy>(
    bytes: &[u8],
    subtype: I,
    endpoint: &ColumnType,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalRange<I>>, SQLError> {
    let flags = *bytes
        .last()
        .filter(|_| bytes.len() >= 5)
        .ok_or_else(|| corrupt("truncated range payload"))?;
    let bounds = &bytes[4..bytes.len() - 1];
    let (range, consumed) = read_bounds(bounds, flags, subtype, endpoint, catalog, control)?;
    if consumed != bounds.len() {
        return Err(corrupt("invalid range bound length"));
    }
    Ok(range)
}

struct Members<'a, I> {
    bytes: &'a [u8],
    bounds: &'a [u8],
    flags: &'a [u8],
    subtype: I,
    endpoint: &'a ColumnType,
    catalog: Option<&'a dyn crate::expr::SQLValueCatalog>,
    index: usize,
    offset: usize,
    previous: usize,
}

impl<'a, I: Copy> Members<'a, I> {
    fn new(
        bytes: &'a [u8],
        subtype: I,
        endpoint: &'a ColumnType,
        catalog: Option<&'a dyn crate::expr::SQLValueCatalog>,
    ) -> Result<Self, SQLError> {
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
        let start = align(flags_end + 4, endpoint)? - 4;
        let bounds = bytes
            .get(start..)
            .ok_or_else(|| corrupt("truncated multirange payload"))?;
        Ok(Self {
            bytes,
            bounds,
            flags: &bytes[flags_start..flags_end],
            subtype,
            endpoint,
            catalog,
            index: 0,
            offset: 0,
            previous: 0,
        })
    }

    fn next(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<CanonicalRange<I>>>, SQLError> {
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
        let (range, consumed) = read_bounds(
            remaining,
            flag,
            self.subtype,
            self.endpoint,
            self.catalog,
            control,
        )?;
        self.offset = self
            .offset
            .checked_add(align(consumed, self.endpoint)?)
            .ok_or_else(|| corrupt("invalid multirange length"))?;
        self.index += 1;
        Ok(Some(range))
    }
}

fn read_bounds<I>(
    bytes: &[u8],
    flags: u8,
    subtype: I,
    endpoint: &ColumnType,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<(Produced<CanonicalRange<I>>, usize), SQLError> {
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
        endpoint,
        catalog,
        control,
    )?;
    let upper = read_bound(
        bytes,
        &mut offset,
        flags & UPPER_INFINITE != 0,
        endpoint,
        catalog,
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
    endpoint: &ColumnType,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Value>>, SQLError> {
    control.check()?;
    if infinite {
        return Ok(control.finish(None, control.empty_reservation())?);
    }
    let value = crate::expr::datums::inline::read(bytes, offset, 0, endpoint, catalog, control)?;
    let (value, memory) = value.into_parts();
    Ok(control.finish(Some(value), memory)?)
}

fn align(offset: usize, endpoint: &ColumnType) -> Result<usize, SQLError> {
    let mask = if pg_type_align(endpoint) == "d" { 7 } else { 3 };
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
