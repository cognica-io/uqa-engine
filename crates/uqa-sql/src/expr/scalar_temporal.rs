//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal, formatting, identity, and UUID built-ins.

use super::{
    coerce_temporal, format_pg_number, format_temporal, generate_random_uuid, generate_uuid_v7,
    out_of_range, pg_to_chrono_fmt, typeof_value, value_to_string, DecimalValue, Result, SQLError,
    TemporalValue, Value,
};
use uqa_core::memory::ProductionControl;

mod immutable;
pub(super) use immutable::eval_temporal_functions_with_control;

#[expect(
    clippy::too_many_lines,
    reason = "builtin dispatch preserves arity, NULL, and error precedence"
)]
pub(super) fn eval_temporal_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    if let Some(result) = super::current_time::eval_current_time(name, args, None) {
        return Some(result);
    }
    if let Some(result) =
        eval_temporal_functions_with_control(name, args, &ProductionControl::uncontrolled())
    {
        return Some(
            result.map(|value| value.into_uncontrolled().expect("ordinary temporal result")),
        );
    }
    const NAMES: &[&str] = &[
        "to_char",
        "to_date",
        "to_number",
        "clock_timestamp",
        "timeofday",
        "typeof",
        "pg_typeof",
        "gen_random_uuid",
        "uuidv4",
        "uuidv7",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Value> {
        match name {
            "to_char" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("to_char takes 2 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let fmt = value_to_string(&args[1]);
                match &args[0] {
                    value @ (Value::Int(_) | Value::Float(_) | Value::Decimal(_)) => {
                        format_pg_number(value, &fmt).map(Value::Str)
                    }
                    Value::Temporal(t) => Ok(Value::Str(format_temporal(t, &fmt)?)),
                    Value::Str(s) => {
                        let temporal = coerce_temporal(&Value::Str(s.clone()))?;
                        Ok(Value::Str(format_temporal(&temporal, &fmt)?))
                    }
                    other => Err(SQLError::TypeMismatch(format!(
                        "to_char: unsupported source {other:?}"
                    ))),
                }
            }
            "to_date" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("to_date takes 2 args".into()));
                }
                let s = value_to_string(&args[0]);
                let fmt = pg_to_chrono_fmt(&value_to_string(&args[1]));
                let date = chrono::NaiveDate::parse_from_str(&s, &fmt)
                    .map_err(|e| SQLError::TypeMismatch(format!("to_date: {e}")))?;
                let epoch = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH.date_naive();
                Ok(Value::Temporal(TemporalValue::Date {
                    days: i32::try_from(date.signed_duration_since(epoch).num_days())
                        .map_err(|_| out_of_range("date"))?,
                }))
            }
            "to_number" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("to_number takes 2 args".into()));
                }
                let s = value_to_string(&args[0]);
                if value_to_string(&args[1]).trim().eq_ignore_ascii_case("RN") {
                    return parse_roman_numeral(&s)
                        .map(DecimalValue::from_i64)
                        .map(Value::Decimal)
                        .ok_or_else(|| SQLError::Routine {
                            sqlstate: "22P02".into(),
                            message: "invalid Roman numeral".into(),
                        });
                }
                let cleaned: String = s
                    .chars()
                    .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
                    .collect();
                DecimalValue::parse(&cleaned)
                    .map(Value::Decimal)
                    .ok_or_else(|| SQLError::TypeMismatch(format!("to_number: {s:?}")))
            }
            "clock_timestamp" => Ok(Value::Temporal(TemporalValue::TimestampTz {
                micros: chrono::Utc::now().timestamp_micros(),
            })),
            "timeofday" => Ok(Value::Str(
                chrono::Utc::now()
                    .format("%a %b %d %H:%M:%S%.6f %Y UTC")
                    .to_string(),
            )),
            "typeof" | "pg_typeof" => Ok(Value::Str(typeof_value(&args[0]))),
            "gen_random_uuid" | "uuidv4" => {
                if !args.is_empty() {
                    return Err(SQLError::TypeMismatch(format!("{name} takes no args")));
                }
                generate_random_uuid().map(Value::Str)
            }
            "uuidv7" => match args {
                [] => generate_uuid_v7(None).map(Value::Str),
                [Value::Temporal(interval @ TemporalValue::Interval { .. })] => {
                    generate_uuid_v7(Some(interval)).map(Value::Str)
                }
                [Value::Null] => Ok(Value::Null),
                [_] => Err(SQLError::TypeMismatch(
                    "uuidv7 shift must be an interval".into(),
                )),
                _ => Err(SQLError::TypeMismatch("uuidv7 takes 0 or 1 args".into())),
            },
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })())
}

fn undefined_uuid_extraction(name: &str, args: &[Value]) -> SQLError {
    let signature = args.iter().map(typeof_value).collect::<Vec<_>>().join(", ");
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!("function {name}({signature}) does not exist"),
    }
}

fn parse_roman_numeral(input: &str) -> Option<i64> {
    const MAX_ROMAN_LEN: usize = 15;

    fn roman_value(byte: u8) -> Option<i64> {
        match byte.to_ascii_uppercase() {
            b'I' => Some(1),
            b'V' => Some(5),
            b'X' => Some(10),
            b'L' => Some(50),
            b'C' => Some(100),
            b'D' => Some(500),
            b'M' => Some(1_000),
            _ => None,
        }
    }

    fn valid_subtraction(current: u8, next: u8) -> bool {
        matches!(
            (current.to_ascii_uppercase(), next.to_ascii_uppercase()),
            (b'I', b'V' | b'X') | (b'X', b'L' | b'C') | (b'C', b'D' | b'M')
        )
    }

    let input = input.as_bytes();
    let mut start = 0;
    while input.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    let roman = input[start..]
        .iter()
        .copied()
        .take(MAX_ROMAN_LEN)
        .take_while(|byte| roman_value(*byte).is_some())
        .map(|byte| byte.to_ascii_uppercase())
        .collect::<Vec<_>>();
    if roman.is_empty() {
        return None;
    }

    let mut total = 0_i64;
    let mut repeat_count = 1;
    let mut v_count = 0;
    let mut l_count = 0;
    let mut d_count = 0;
    let mut subtraction_encountered = false;
    let mut last_subtracted_value = 0;
    let mut index = 0;
    while index < roman.len() {
        let current = roman[index];
        let current_value = roman_value(current)?;
        if subtraction_encountered && current_value >= last_subtracted_value {
            return None;
        }
        if v_count > 0 && current_value >= 5
            || l_count > 0 && current_value >= 50
            || d_count > 0 && current_value >= 500
        {
            return None;
        }
        match current {
            b'V' => v_count += 1,
            b'L' => l_count += 1,
            b'D' => d_count += 1,
            _ => {}
        }

        if let Some(next) = roman.get(index + 1).copied() {
            let next_value = roman_value(next)?;
            if current_value < next_value {
                if !valid_subtraction(current, next) || repeat_count > 1 {
                    return None;
                }
                if v_count > 0 && next_value >= 5
                    || l_count > 0 && next_value >= 50
                    || d_count > 0 && next_value >= 500
                {
                    return None;
                }
                match next {
                    b'V' => v_count += 1,
                    b'L' => l_count += 1,
                    b'D' => d_count += 1,
                    _ => {}
                }
                index += 2;
                repeat_count = 1;
                subtraction_encountered = true;
                last_subtracted_value = current_value;
                total += next_value - current_value;
                continue;
            }
            if current == next {
                repeat_count += 1;
                if repeat_count > 3 {
                    return None;
                }
            } else {
                repeat_count = 1;
            }
        }
        total += current_value;
        index += 1;
    }
    Some(total)
}
