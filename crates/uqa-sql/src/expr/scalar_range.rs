//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` range and multirange scalar functions and lowered operators.

use super::{
    conversion::value_to_string_with_control,
    range::{
        multirange_from_produced_ranges, parse_multirange_with_control, parse_range_with_control,
    },
    CanonicalMultirange, CanonicalRange, Result, SQLError, Value,
};
use crate::ast::{RangeFunctionOperation, RangeSubtype};
use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

mod sets;
use sets::RangeSet;

const SUBTYPES: &[RangeSubtype] = &[
    RangeSubtype::Integer,
    RangeSubtype::BigInteger,
    RangeSubtype::Numeric,
    RangeSubtype::Date,
    RangeSubtype::Timestamp,
    RangeSubtype::TimestampTz,
];

pub(super) fn eval_range_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    eval_range_functions_with_control(name, args, &ProductionControl::uncontrolled())
        .map(|result| result.map(|value| value.into_uncontrolled().expect("ordinary range result")))
}

pub(super) fn eval_range_functions_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    if let Some(subtype) = SUBTYPES
        .iter()
        .copied()
        .find(|subtype| name == subtype.range_name())
    {
        return Some(range_constructor(subtype, args, control));
    }
    if let Some(subtype) = SUBTYPES
        .iter()
        .copied()
        .find(|subtype| name == subtype.multirange_name())
    {
        return Some(multirange_constructor(subtype, args, control));
    }
    None
}

#[cfg(test)]
pub(super) fn eval_dispatched_range_function(
    operation: RangeFunctionOperation,
    subtype: RangeSubtype,
    multirange: bool,
    args: &[Value],
) -> Result<Value> {
    Ok(eval_dispatched_range_function_with_control(
        operation,
        subtype,
        multirange,
        args,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary dispatched range result"))
}

pub(super) fn eval_dispatched_range_function_with_control(
    operation: RangeFunctionOperation,
    subtype: RangeSubtype,
    multirange: bool,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    let operation = match operation {
        RangeFunctionOperation::Lower => "lower",
        RangeFunctionOperation::Upper => "upper",
        RangeFunctionOperation::IsEmpty => "isempty",
        RangeFunctionOperation::LowerInclusive => "lower_inc",
        RangeFunctionOperation::UpperInclusive => "upper_inc",
        RangeFunctionOperation::LowerInfinite => "lower_inf",
        RangeFunctionOperation::UpperInfinite => "upper_inf",
        RangeFunctionOperation::Merge => "merge",
        RangeFunctionOperation::Multirange => "multirange",
        RangeFunctionOperation::Overlap => "overlap",
        RangeFunctionOperation::Contains => "contains",
        RangeFunctionOperation::ContainedBy => "contained_by",
        RangeFunctionOperation::Adjacent => "adjacent",
    };
    for argument in args {
        control.check()?;
        if matches!(argument, Value::Null) {
            return inline_value(Value::Null, control);
        }
    }
    match operation {
        "lower" | "upper" | "isempty" | "lower_inc" | "upper_inc" | "lower_inf" | "upper_inf" => {
            accessor(operation, subtype, multirange, args, control)
        }
        "merge" => merge(subtype, multirange, args, control),
        "multirange" => multirange_constructor(subtype, args, control),
        "overlap" | "contains" | "contained_by" | "adjacent" => {
            operator(operation, subtype, multirange, args, control)
        }
        _ => Err(SQLError::Internal(format!(
            "unknown range dispatch operation `{operation}`"
        ))),
    }
}

fn range_constructor(
    subtype: RangeSubtype,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if !matches!(args.len(), 2 | 3) {
        return Err(SQLError::TypeMismatch(format!(
            "{} takes 2 or 3 arguments",
            subtype.range_name()
        )));
    }
    let bounds = match args.get(2) {
        None => "[)",
        Some(Value::Str(bounds) | Value::FixedChar(bounds)) => bounds,
        Some(other) => {
            return Err(SQLError::TypeMismatch(format!(
                "range bounds must be text, got {other:?}"
            )))
        }
    };
    if bounds.len() != 2
        || !matches!(bounds.as_bytes()[0], b'[' | b'(')
        || !matches!(bounds.as_bytes()[1], b']' | b')')
    {
        return Err(SQLError::Routine {
            sqlstate: "22000".into(),
            message: format!("invalid range bound flags: \"{bounds}\""),
        });
    }
    let mut text = ProductionString::new(*control);
    text.push_str(&bounds[..1])?;
    text.push_str(&value_to_string_with_control(&args[0], control)?)?;
    text.push(',')?;
    text.push_str(&value_to_string_with_control(&args[1], control)?)?;
    text.push_str(&bounds[1..])?;
    let text = text.finish()?;
    let range = parse_range_with_control(&text, subtype, control)?;
    text_value(range.to_text_with_control(control)?, control)
}

fn multirange_constructor(
    subtype: RangeSubtype,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    let mut ranges = ProductionVec::new(*control);
    for argument in args {
        control.check()?;
        match argument {
            Value::Str(text) | Value::FixedChar(text) => {
                ranges.push_produced(parse_range_with_control(text, subtype, control)?)?;
            }
            Value::Array(array) => {
                for value in array.elements() {
                    control.check()?;
                    let (Value::Str(text) | Value::FixedChar(text)) = value else {
                        return Err(SQLError::TypeMismatch(format!(
                            "{} variadic input must contain ranges",
                            subtype.multirange_name()
                        )));
                    };
                    ranges.push_produced(parse_range_with_control(text, subtype, control)?)?;
                }
            }
            Value::Null => return inline_value(Value::Null, control),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "{} requires range arguments, got {other:?}",
                    subtype.multirange_name()
                )))
            }
        }
    }
    let ranges = multirange_from_produced_ranges(subtype, ranges.finish()?, control)?;
    text_value(ranges.to_text_with_control(control)?, control)
}

fn accessor(
    operation: &str,
    subtype: RangeSubtype,
    multirange: bool,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let [argument] = args else {
        return Err(SQLError::TypeMismatch(format!(
            "{operation} takes 1 argument"
        )));
    };
    let ranges = RangeSet::parse(range_text(argument)?, subtype, multirange, control)?;
    if operation == "isempty" {
        return inline_value(
            Value::Bool(if multirange {
                ranges.ranges().is_empty()
            } else {
                ranges
                    .ranges()
                    .first()
                    .is_some_and(CanonicalRange::is_empty)
            }),
            control,
        );
    }
    let range = match operation {
        "lower" | "lower_inc" | "lower_inf" => ranges.ranges().first(),
        "upper" | "upper_inc" | "upper_inf" => ranges.ranges().last(),
        _ => None,
    };
    let Some(range) = range.filter(|range| !range.is_empty()) else {
        return inline_value(
            match operation {
                "lower" | "upper" => Value::Null,
                _ => Value::Bool(false),
            },
            control,
        );
    };
    match operation {
        "lower" | "upper" => {
            let bound = if operation == "lower" {
                range.lower()
            } else {
                range.upper()
            };
            match bound {
                Some(value) => Ok(control.copy_value(value)?),
                None => inline_value(Value::Null, control),
            }
        }
        "lower_inc" => inline_value(
            Value::Bool(range.lower().is_some() && range.lower_inclusive()),
            control,
        ),
        "upper_inc" => inline_value(
            Value::Bool(range.upper().is_some() && range.upper_inclusive()),
            control,
        ),
        "lower_inf" => inline_value(Value::Bool(range.lower().is_none()), control),
        "upper_inf" => inline_value(Value::Bool(range.upper().is_none()), control),
        _ => unreachable!(),
    }
}

fn merge(
    subtype: RangeSubtype,
    multirange: bool,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let merged = if multirange {
        let [argument] = args else {
            return Err(SQLError::TypeMismatch(
                "range_merge(multirange) takes 1 argument".into(),
            ));
        };
        parse_multirange_with_control(range_text(argument)?, subtype, control)?
            .merge_cover_with_control(control)?
    } else {
        let [left, right] = args else {
            return Err(SQLError::TypeMismatch(
                "range_merge(range, range) takes 2 arguments".into(),
            ));
        };
        let left = parse_range_with_control(range_text(left)?, subtype, control)?;
        let right = parse_range_with_control(range_text(right)?, subtype, control)?;
        left.merge_cover_with_control(&right, control)?
    };
    text_value(merged.to_text_with_control(control)?, control)
}

fn operator(
    operation: &str,
    subtype: RangeSubtype,
    left_multirange: bool,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let [left, right] = args else {
        return Err(SQLError::TypeMismatch(format!(
            "range {operation} operator takes 2 arguments"
        )));
    };
    let left = RangeSet::parse(range_text(left)?, subtype, left_multirange, control)?;
    let right = RangeSet::parse_auto(range_text(right)?, subtype, control)?;
    inline_value(
        Value::Bool(match operation {
            "overlap" => left.overlaps(&right, control)?,
            "contains" => left.contains(&right, control)?,
            "contained_by" => right.contains(&left, control)?,
            "adjacent" => left.adjacent(&right, control)?,
            _ => unreachable!(),
        }),
        control,
    )
}

fn range_text(value: &Value) -> Result<&str> {
    match value {
        Value::Str(text) | Value::FixedChar(text) => Ok(text),
        other => Err(SQLError::TypeMismatch(format!(
            "range function requires a range value, got {other:?}"
        ))),
    }
}

fn text_value(text: Produced<String>, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let (text, memory) = text.into_parts();
    Ok(control.finish(Value::Str(text), memory)?)
}

fn inline_value(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    debug_assert!(matches!(value, Value::Null | Value::Bool(_)));
    Ok(control.finish(value, control.empty_reservation())?)
}

#[cfg(test)]
mod tests;
