//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact range keys frame SQL-owned normalized bounds in the existing scalar key domains.

use super::{encode_bytes, encode_leaf, encode_len, output::KeyOutput, ExecResult, Value};
use uqa_core::memory::ProductionControl;
use uqa_sql::expr::{CanonicalRange, RangeComparisonValue};

pub(super) fn encode_declared_range(
    value: &Value,
    oid: Option<u32>,
    output: &mut impl KeyOutput,
) -> ExecResult<bool> {
    if !output.uses_declared_field_types() || !matches!(value, Value::Str(_) | Value::Datum(_)) {
        return Ok(false);
    }
    let oid = match oid {
        Some(oid) => Some(oid),
        None => RangeComparisonValue::declared_physical_oid(value, output.enum_catalog())?,
    };
    let Some(oid) = oid else { return Ok(false) };
    let read = output.control().cloned();
    let control = read
        .as_ref()
        .map_or_else(ProductionControl::uncontrolled, |read| {
            ProductionControl::new(read.memory(), read.cancellation(), read.cancellation())
        });
    let Some(value) = RangeComparisonValue::from_declared_with_control(
        value,
        oid,
        output.enum_catalog(),
        &control,
    )?
    else {
        return Ok(false);
    };
    output.encoded_range_field();
    match value {
        RangeComparisonValue::Range(value) => {
            output.push_byte(18)?;
            encode_range(&value, output)?;
        }
        RangeComparisonValue::Multirange(value) => {
            output.push_byte(19)?;
            encode_len(value.ranges().len(), output)?;
            for range in value.ranges() {
                output.check()?;
                encode_range(range, output)?;
            }
        }
        RangeComparisonValue::DeclaredRange(value) => {
            output.push_byte(21)?;
            output.extend_bytes(&value.identity().to_be_bytes())?;
            encode_declared_bounds(&value, output, &control)?;
        }
        RangeComparisonValue::DeclaredMultirange(value) => {
            output.push_byte(22)?;
            output.extend_bytes(&value.identity().to_be_bytes())?;
            encode_len(value.ranges().len(), output)?;
            for range in value.ranges() {
                output.check()?;
                encode_declared_bounds(range, output, &control)?;
            }
        }
    }
    Ok(true)
}

fn encode_range(value: &CanonicalRange, output: &mut impl KeyOutput) -> ExecResult<()> {
    output.push_byte(u8::from(value.is_empty()))?;
    if value.is_empty() {
        return Ok(());
    }
    for (bound, inclusive) in [
        (value.lower(), value.lower_inclusive()),
        (value.upper(), value.upper_inclusive()),
    ] {
        output.push_byte(u8::from(inclusive))?;
        output.push_byte(u8::from(bound.is_some()))?;
        if let Some(bound) = bound {
            encode_leaf(bound, output)?;
        }
    }
    Ok(())
}

fn encode_declared_bounds(
    value: &CanonicalRange<u32>,
    output: &mut impl KeyOutput,
    control: &ProductionControl<'_>,
) -> ExecResult<()> {
    output.push_byte(u8::from(value.is_empty()))?;
    if value.is_empty() {
        return Ok(());
    }
    for (bound, inclusive) in [
        (value.lower(), value.lower_inclusive()),
        (value.upper(), value.upper_inclusive()),
    ] {
        output.push_byte(u8::from(inclusive))?;
        output.push_byte(u8::from(bound.is_some()))?;
        if let Some(bound) = bound {
            let key = output
                .enum_catalog()
                .ok_or_else(|| {
                    uqa_sql::SQLError::Internal(
                        "selected range equality-key support is unavailable".into(),
                    )
                })?
                .range_bound_equality_key(*value.identity(), bound, control)?;
            encode_bytes(&key, output)?;
        }
    }
    Ok(())
}
