//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` range and multirange scalar functions and lowered operators.

use super::{
    multirange_from_ranges, parse_multirange, parse_range, value_to_string, CanonicalRange, Result,
    SQLError, Value,
};
use crate::ast::{RangeFunctionOperation, RangeSubtype};

const SUBTYPES: &[RangeSubtype] = &[
    RangeSubtype::Integer,
    RangeSubtype::BigInteger,
    RangeSubtype::Numeric,
    RangeSubtype::Date,
    RangeSubtype::Timestamp,
    RangeSubtype::TimestampTz,
];

pub(super) fn eval_range_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    if let Some(subtype) = SUBTYPES
        .iter()
        .copied()
        .find(|subtype| name == subtype.range_name())
    {
        return Some(range_constructor(subtype, args));
    }
    if let Some(subtype) = SUBTYPES
        .iter()
        .copied()
        .find(|subtype| name == subtype.multirange_name())
    {
        return Some(multirange_constructor(subtype, args));
    }
    None
}

pub(super) fn eval_dispatched_range_function(
    operation: RangeFunctionOperation,
    subtype: RangeSubtype,
    multirange: bool,
    args: &[Value],
) -> Result<Value> {
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
    (|| {
        if args.iter().any(|argument| matches!(argument, Value::Null)) {
            return Ok(Value::Null);
        }
        match operation {
            "lower" | "upper" | "isempty" | "lower_inc" | "upper_inc" | "lower_inf"
            | "upper_inf" => accessor(operation, subtype, multirange, args),
            "merge" => merge(subtype, multirange, args),
            "multirange" => multirange_constructor(subtype, args),
            "overlap" | "contains" | "contained_by" | "adjacent" => {
                operator(operation, subtype, multirange, args)
            }
            _ => Err(SQLError::Internal(format!(
                "unknown range dispatch operation `{operation}`"
            ))),
        }
    })()
}

fn range_constructor(subtype: RangeSubtype, args: &[Value]) -> Result<Value> {
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
    parse_range(
        &format!(
            "{}{},{}{}",
            &bounds[..1],
            constructor_bound(&args[0]),
            constructor_bound(&args[1]),
            &bounds[1..]
        ),
        subtype,
    )
    .map(|range| Value::Str(range.to_text()))
}

fn constructor_bound(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        value => value_to_string(value),
    }
}

fn multirange_constructor(subtype: RangeSubtype, args: &[Value]) -> Result<Value> {
    let mut ranges = Vec::new();
    for argument in args {
        match argument {
            Value::Str(text) | Value::FixedChar(text) => ranges.push(parse_range(text, subtype)?),
            Value::Array(array) => {
                for value in array.elements() {
                    let (Value::Str(text) | Value::FixedChar(text)) = value else {
                        return Err(SQLError::TypeMismatch(format!(
                            "{} variadic input must contain ranges",
                            subtype.multirange_name()
                        )));
                    };
                    ranges.push(parse_range(text, subtype)?);
                }
            }
            Value::Null => return Ok(Value::Null),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "{} requires range arguments, got {other:?}",
                    subtype.multirange_name()
                )))
            }
        }
    }
    Ok(Value::Str(
        multirange_from_ranges(subtype, ranges).to_text(),
    ))
}

fn accessor(
    operation: &str,
    subtype: RangeSubtype,
    multirange: bool,
    args: &[Value],
) -> Result<Value> {
    let [argument] = args else {
        return Err(SQLError::TypeMismatch(format!(
            "{operation} takes 1 argument"
        )));
    };
    let text = range_text(argument)?;
    let range = if multirange {
        let multirange = parse_multirange(text, subtype)?;
        match operation {
            "lower" | "lower_inc" | "lower_inf" => multirange.ranges().first().cloned(),
            "upper" | "upper_inc" | "upper_inf" => multirange.ranges().last().cloned(),
            "isempty" => return Ok(Value::Bool(multirange.ranges().is_empty())),
            _ => None,
        }
    } else {
        Some(parse_range(text, subtype)?)
    };
    if operation == "isempty" {
        return Ok(Value::Bool(
            range.as_ref().is_some_and(CanonicalRange::is_empty),
        ));
    }
    let Some(range) = range.filter(|range| !range.is_empty()) else {
        return Ok(match operation {
            "lower" | "upper" => Value::Null,
            _ => Value::Bool(false),
        });
    };
    Ok(match operation {
        "lower" => range.lower().cloned().unwrap_or(Value::Null),
        "upper" => range.upper().cloned().unwrap_or(Value::Null),
        "lower_inc" => Value::Bool(range.lower().is_some() && range.lower_inclusive()),
        "upper_inc" => Value::Bool(range.upper().is_some() && range.upper_inclusive()),
        "lower_inf" => Value::Bool(range.lower().is_none()),
        "upper_inf" => Value::Bool(range.upper().is_none()),
        _ => unreachable!(),
    })
}

fn merge(subtype: RangeSubtype, multirange: bool, args: &[Value]) -> Result<Value> {
    let merged = if multirange {
        let [argument] = args else {
            return Err(SQLError::TypeMismatch(
                "range_merge(multirange) takes 1 argument".into(),
            ));
        };
        parse_multirange(range_text(argument)?, subtype)?.merge_cover()
    } else {
        let [left, right] = args else {
            return Err(SQLError::TypeMismatch(
                "range_merge(range, range) takes 2 arguments".into(),
            ));
        };
        parse_range(range_text(left)?, subtype)?
            .merge_cover(&parse_range(range_text(right)?, subtype)?)
    };
    Ok(Value::Str(merged.to_text()))
}

fn operator(
    operation: &str,
    subtype: RangeSubtype,
    left_multirange: bool,
    args: &[Value],
) -> Result<Value> {
    let [left, right] = args else {
        return Err(SQLError::TypeMismatch(format!(
            "range {operation} operator takes 2 arguments"
        )));
    };
    let left = RangeSet::parse(range_text(left)?, subtype, left_multirange)?;
    let right = RangeSet::parse_auto(range_text(right)?, subtype)?;
    Ok(Value::Bool(match operation {
        "overlap" => left.overlaps(&right),
        "contains" => left.contains(&right),
        "contained_by" => right.contains(&left),
        "adjacent" => left.adjacent(&right),
        _ => unreachable!(),
    }))
}

struct RangeSet(Vec<CanonicalRange>);

impl RangeSet {
    fn parse(text: &str, subtype: RangeSubtype, multirange: bool) -> Result<Self> {
        if multirange {
            parse_multirange(text, subtype).map(|value| Self(value.ranges().to_vec()))
        } else {
            parse_range(text, subtype).map(|value| Self(vec![value]))
        }
    }

    fn parse_auto(text: &str, subtype: RangeSubtype) -> Result<Self> {
        Self::parse(text, subtype, text.trim_start().starts_with('{'))
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.0
            .iter()
            .any(|left| other.0.iter().any(|right| left.overlaps(right)))
    }

    fn contains(&self, other: &Self) -> bool {
        other
            .0
            .iter()
            .all(|right| self.0.iter().any(|left| left.contains_range(right)))
    }

    fn adjacent(&self, other: &Self) -> bool {
        !self.overlaps(other)
            && self
                .0
                .iter()
                .any(|left| other.0.iter().any(|right| left.adjacent(right)))
    }
}

fn range_text(value: &Value) -> Result<&str> {
    match value {
        Value::Str(text) | Value::FixedChar(text) => Ok(text),
        other => Err(SQLError::TypeMismatch(format!(
            "range function requires a range value, got {other:?}"
        ))),
    }
}
