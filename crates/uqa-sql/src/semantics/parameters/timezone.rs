//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `TimeZone` settings use SQL signs for numeric hours and POSIX signs for named rules.

use super::definition::ParameterDefinition;
use super::units::c_strtod;
use super::value::invalid_value_message;
use crate::SQLError;
use uqa_core::{TemporalTimeZone, TemporalValue, Value};

pub(super) fn parse_setting(
    definition: &ParameterDefinition,
    raw: &str,
) -> Result<String, SQLError> {
    let invalid = |detail: Option<&str>| SQLError::Diagnostic {
        sqlstate: "22023".into(),
        message: invalid_value_message(definition, raw),
        detail: detail.map(str::to_string),
        hint: None,
    };
    if let Some(rest) = raw
        .get(..8)
        .filter(|prefix| prefix.eq_ignore_ascii_case("interval"))
        .map(|_| raw[8..].trim_start_matches(|ch: char| ch.is_ascii_whitespace()))
    {
        let text = rest
            .strip_prefix('\'')
            .and_then(|text| text.strip_suffix('\''))
            .filter(|text| !text.contains('\''))
            .ok_or_else(|| invalid(None))?;
        let interval = crate::expr::cast_value(&Value::Str(text.to_string()), "interval")?;
        let Value::Temporal(TemporalValue::Interval {
            months,
            days,
            micros,
        }) = interval
        else {
            unreachable!("interval input preserves its temporal carrier");
        };
        if months != 0 {
            return Err(invalid(Some(
                "Cannot specify months in time zone interval.",
            )));
        }
        if days != 0 {
            return Err(invalid(Some("Cannot specify days in time zone interval.")));
        }
        return fixed_setting(micros / 1_000_000)
            .ok_or_else(|| invalid(Some("UTC timezone offset is out of range.")));
    }
    let (hours, end, _) = c_strtod(raw.as_bytes());
    if end != 0 && end == raw.len() {
        let seconds = (hours * 3_600.0).trunc();
        if seconds.is_infinite() || seconds < f64::from(i32::MIN) || seconds > f64::from(i32::MAX) {
            return Err(invalid(Some("UTC timezone offset is out of range.")));
        }
        return fixed_setting(seconds as i64)
            .ok_or_else(|| invalid(Some("UTC timezone offset is out of range.")));
    }
    TemporalTimeZone::named(raw).ok_or_else(|| invalid(None))?;
    Ok(TemporalTimeZone::canonical_name(raw)
        .map_or_else(|| raw.to_ascii_uppercase(), str::to_string))
}

fn fixed_setting(seconds_east: i64) -> Option<String> {
    let seconds = seconds_east.unsigned_abs();
    let mut offset = format!("{:02}", seconds / 3_600);
    if seconds % 3_600 != 0 {
        offset.push_str(&format!(":{:02}", seconds / 60 % 60));
        if seconds % 60 != 0 {
            offset.push_str(&format!(":{:02}", seconds % 60));
        }
    }
    let (sql_sign, posix_sign) = if seconds_east < 0 {
        ('-', '+')
    } else {
        ('+', '-')
    };
    let name = format!("<{sql_sign}{offset}>{posix_sign}{offset}");
    TemporalTimeZone::named(&name).map(|_| name)
}

#[cfg(test)]
mod tests;
