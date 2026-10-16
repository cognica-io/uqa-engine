//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected bit casts reuse the packed input and length-coercion owner.

use super::{
    canonical_cast_source_with_control, postgres_type_display_name, undefined_cast, Produced,
    ProductionControl, Result, Value,
};
use crate::{
    expr::bit_string::{self, BitStringKind},
    ColumnType,
};

pub(super) fn cast(
    value: &Value,
    ty: &ColumnType,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (kind, length) = match ty {
        ColumnType::Bit(length) => (BitStringKind::Fixed, *length),
        ColumnType::VarBit(length) => (BitStringKind::Varying, *length),
        _ => unreachable!("bit target was selected"),
    };
    let modifier = length.map_or(-1, |length| length as i32);
    let source = canonical_cast_source_with_control(source_ty, value, control)?;
    let bits = match value {
        Value::BitString(bits) => {
            bit_string::coerce_with_control(bits, kind, modifier, true, control)?
        }
        Value::Str(text) | Value::FixedChar(text)
            if matches!(
                source.as_str(),
                "unknown" | "text" | "name" | "varchar" | "bpchar"
            ) =>
        {
            let input = bit_string::input_with_control(text, kind, -1, control)?;
            bit_string::coerce_with_control(&input, kind, modifier, true, control)?
        }
        Value::Int(integer)
            if kind == BitStringKind::Fixed && matches!(source.as_str(), "int4" | "int8") =>
        {
            bit_string::from_integer_with_control(*integer, modifier, control)?
        }
        _ => {
            return Err(undefined_cast(
                postgres_type_display_name(&source),
                if kind == BitStringKind::Fixed {
                    "bit"
                } else {
                    "bit varying"
                },
            ))
        }
    };
    let (bits, memory) = bits.into_parts();
    Ok(control.finish(Value::BitString(bits), memory)?)
}

pub(super) fn integer(
    value: &uqa_core::BitStringValue,
    target: &str,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    control.check()?;
    if let Some(source) = source_ty {
        let source = crate::ColumnType::from_sql_name_with_control(source, control)?;
        if !matches!(*source, ColumnType::Bit(_)) {
            return Err(undefined_cast(&source.regtype_name(), target));
        }
    }
    let integer = match target {
        "integer" => i64::from(bit_string::to_integer(value)?),
        "bigint" => bit_string::to_bigint(value)?,
        _ => return Err(undefined_cast("bit", target)),
    };
    Ok(Value::Int(integer))
}
