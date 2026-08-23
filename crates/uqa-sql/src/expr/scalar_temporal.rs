//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal, formatting, identity, and UUID built-ins.

use super::{
    age_between, coerce_temporal, date_trunc_value, extract_from_value, extract_uuid_timestamp,
    extract_uuid_version, float_to_i64_rounded, format_pg_number, format_temporal,
    generate_random_uuid, generate_uuid_v7, make_timestamp, out_of_range, pg_to_chrono_fmt, to_f64,
    to_i64, typeof_value, value_to_string, DecimalValue, Result, SQLError, TemporalValue, Value,
};

pub(super) fn eval_temporal_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    const NAMES: &[&str] = &[
        "now",
        "current_timestamp",
        "current_date",
        "to_timestamp",
        "extract",
        "date_part",
        "age",
        "date_trunc",
        "make_timestamp",
        "make_date",
        "make_interval",
        "justify_hours",
        "to_char",
        "to_date",
        "to_number",
        "isfinite",
        "clock_timestamp",
        "statement_timestamp",
        "timeofday",
        "typeof",
        "pg_typeof",
        "gen_random_uuid",
        "uuidv4",
        "uuidv7",
        "uuid_extract_version",
        "uuid_extract_timestamp",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Value> {
        match name {
            "now" | "current_timestamp" => Ok(Value::Temporal(TemporalValue::TimestampTz {
                micros: chrono::Utc::now().timestamp_micros(),
            })),
            "current_date" => {
                let micros = chrono::Utc::now().timestamp_micros();
                Ok(Value::Temporal(TemporalValue::Date {
                    days: (micros.div_euclid(86_400_000_000)) as i32,
                }))
            }
            "to_timestamp" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("to_timestamp takes 1 arg".into()));
                }
                let secs = to_f64(&args[0])?;
                Ok(Value::Temporal(TemporalValue::TimestampTz {
                    micros: float_to_i64_rounded(secs * 1e6, "timestamp")?,
                }))
            }
            "extract" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(
                        "extract takes 2 args (field, ts)".into(),
                    ));
                }
                let field = value_to_string(&args[0]).to_ascii_lowercase();
                extract_from_value(&field, &args[1], true)
            }
            "date_part" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(
                        "date_part takes 2 args (field, ts)".into(),
                    ));
                }
                let field = value_to_string(&args[0]).to_ascii_lowercase();
                extract_from_value(&field, &args[1], false)
            }
            "age" => {
                let (a, b) = match args.len() {
                    // One-argument age() measures against today's midnight.
                    1 => {
                        let micros = chrono::Utc::now().timestamp_micros();
                        let midnight = micros.div_euclid(86_400_000_000) * 86_400_000_000;
                        (
                            coerce_temporal(&args[0])?,
                            TemporalValue::Timestamp { micros: midnight },
                        )
                    }
                    2 => (coerce_temporal(&args[0])?, coerce_temporal(&args[1])?),
                    _ => return Err(SQLError::TypeMismatch("age takes 1-2 args".into())),
                };
                age_between(&a, &b)
            }
            "date_trunc" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("date_trunc takes 2 args".into()));
                }
                let unit = value_to_string(&args[0]).to_ascii_lowercase();
                date_trunc_value(&unit, &args[1])
            }
            "make_timestamp" => {
                if !(6..=7).contains(&args.len()) {
                    return Err(SQLError::TypeMismatch(
                        "make_timestamp takes 6-7 args".into(),
                    ));
                }
                let year = i32::try_from(to_i64(&args[0])?).map_err(|_| out_of_range("date"))?;
                let month = u32::try_from(to_i64(&args[1])?).map_err(|_| out_of_range("date"))?;
                let day = u32::try_from(to_i64(&args[2])?).map_err(|_| out_of_range("date"))?;
                let hour = u32::try_from(to_i64(&args[3])?).map_err(|_| out_of_range("time"))?;
                let minute = u32::try_from(to_i64(&args[4])?).map_err(|_| out_of_range("time"))?;
                let second = to_f64(&args[5])?;
                make_timestamp(year, month, day, hour, minute, second)
            }
            "make_date" => {
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("make_date takes 3 args".into()));
                }
                let year = i32::try_from(to_i64(&args[0])?).map_err(|_| out_of_range("date"))?;
                let month = u32::try_from(to_i64(&args[1])?).map_err(|_| out_of_range("date"))?;
                let day = u32::try_from(to_i64(&args[2])?).map_err(|_| out_of_range("date"))?;
                let epoch = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH.date_naive();
                let date = chrono::NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| {
                    SQLError::Routine {
                        sqlstate: "22008".into(),
                        message: format!(
                            "date field value out of range: {year:04}-{month:02}-{day:02}"
                        ),
                    }
                })?;
                Ok(Value::Temporal(TemporalValue::Date {
                    days: i32::try_from(date.signed_duration_since(epoch).num_days())
                        .map_err(|_| out_of_range("date"))?,
                }))
            }
            "make_interval" => {
                // make_interval(years, months, weeks, days, hours, mins,
                // secs) -> PostgreSQL's months/days/micros interval model.
                let years = args.first().map(to_i64).transpose()?.unwrap_or(0);
                let months = args.get(1).map(to_i64).transpose()?.unwrap_or(0);
                let weeks = args.get(2).map(to_i64).transpose()?.unwrap_or(0);
                let days = args.get(3).map(to_i64).transpose()?.unwrap_or(0);
                let hours = args.get(4).map(to_i64).transpose()?.unwrap_or(0);
                let mins = args.get(5).map(to_i64).transpose()?.unwrap_or(0);
                let secs = args.get(6).map(to_f64).transpose()?.unwrap_or(0.0);
                let total_months = years
                    .checked_mul(12)
                    .and_then(|value| value.checked_add(months))
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| out_of_range("interval"))?;
                let total_days = weeks
                    .checked_mul(7)
                    .and_then(|value| value.checked_add(days))
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| out_of_range("interval"))?;
                let whole_micros = hours
                    .checked_mul(3_600)
                    .and_then(|value| {
                        mins.checked_mul(60)
                            .and_then(|mins| value.checked_add(mins))
                    })
                    .and_then(|value| value.checked_mul(1_000_000))
                    .ok_or_else(|| out_of_range("interval"))?;
                let fractional_micros = float_to_i64_rounded(secs * 1e6, "interval")?;
                let micros = whole_micros
                    .checked_add(fractional_micros)
                    .ok_or_else(|| out_of_range("interval"))?;
                Ok(Value::Temporal(TemporalValue::Interval {
                    months: total_months,
                    days: total_days,
                    micros,
                }))
            }
            "justify_hours" => {
                if let Some(Value::Temporal(TemporalValue::Interval {
                    months,
                    days,
                    micros,
                })) = args.first()
                {
                    let extra_days = micros.div_euclid(86_400_000_000);
                    let extra_days =
                        i32::try_from(extra_days).map_err(|_| out_of_range("interval"))?;
                    let days = days
                        .checked_add(extra_days)
                        .ok_or_else(|| out_of_range("interval"))?;
                    return Ok(Value::Temporal(TemporalValue::Interval {
                        months: *months,
                        days,
                        micros: micros.rem_euclid(86_400_000_000),
                    }));
                }
                Err(SQLError::TypeMismatch(
                    "justify_hours takes an interval".into(),
                ))
            }
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
            "isfinite" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("isfinite takes 1 arg".into()));
                }
                match &args[0] {
                    Value::Float(f) => Ok(Value::Bool(f.is_finite())),
                    // The temporal model has no infinity values, so every
                    // date / timestamp / interval is finite.
                    Value::Int(_) | Value::Decimal(_) | Value::Str(_) | Value::Temporal(_) => {
                        Ok(Value::Bool(true))
                    }
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "isfinite: unsupported {other:?}"
                    ))),
                }
            }
            "clock_timestamp" | "statement_timestamp" => {
                Ok(Value::Temporal(TemporalValue::TimestampTz {
                    micros: chrono::Utc::now().timestamp_micros(),
                }))
            }
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
            "uuid_extract_version" => match args {
                [uuid @ (Value::Str(_) | Value::FixedChar(_))] => extract_uuid_version(uuid),
                _ => Err(undefined_uuid_extraction(name, args)),
            },
            "uuid_extract_timestamp" => match args {
                [uuid @ (Value::Str(_) | Value::FixedChar(_))] => extract_uuid_timestamp(uuid),
                _ => Err(undefined_uuid_extraction(name, args)),
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
