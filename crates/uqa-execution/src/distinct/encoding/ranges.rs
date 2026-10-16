//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact range keys frame SQL-owned normalized bounds in the existing scalar key domains.

use super::{encode_leaf, encode_len, output::KeyOutput, ExecResult, Value};
use uqa_core::memory::ProductionControl;
use uqa_sql::expr::{CanonicalRange, RangeComparisonValue};

pub(super) fn encode_declared_range(
    value: &Value,
    oid: Option<u32>,
    output: &mut impl KeyOutput,
) -> ExecResult<bool> {
    let Some(oid) = oid else { return Ok(false) };
    if !output.uses_declared_field_types() || !matches!(value, Value::Str(_)) {
        return Ok(false);
    }
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
