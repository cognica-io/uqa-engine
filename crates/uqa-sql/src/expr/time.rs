//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal, formatting, UUID, and hex helpers for scalar functions.

use uqa_core::Value;

use crate::error::{Result, SQLError};

pub(super) fn date_trunc(unit: &str, dt: &chrono::DateTime<chrono::Utc>) -> Result<Value> {
    use chrono::{Datelike, NaiveDate, TimeZone, Timelike, Utc};
    let truncated = match unit {
        "year" => Utc
            .with_ymd_and_hms(dt.year(), 1, 1, 0, 0, 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad year".into()))?,
        "quarter" => {
            let q_start_month = ((dt.month() - 1) / 3) * 3 + 1;
            Utc.with_ymd_and_hms(dt.year(), q_start_month, 1, 0, 0, 0)
                .single()
                .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad quarter".into()))?
        }
        "month" => Utc
            .with_ymd_and_hms(dt.year(), dt.month(), 1, 0, 0, 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad month".into()))?,
        "week" => {
            // Truncate to the Monday of the ISO week.
            let weekday_offset = dt.weekday().num_days_from_monday() as i64;
            let date = NaiveDate::from_ymd_opt(dt.year(), dt.month(), dt.day())
                .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad week".into()))?
                - chrono::Duration::days(weekday_offset);
            let naive = date.and_hms_opt(0, 0, 0).unwrap();
            Utc.from_utc_datetime(&naive)
        }
        "day" => Utc
            .with_ymd_and_hms(dt.year(), dt.month(), dt.day(), 0, 0, 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad day".into()))?,
        "hour" => Utc
            .with_ymd_and_hms(dt.year(), dt.month(), dt.day(), dt.hour(), 0, 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad hour".into()))?,
        "minute" => Utc
            .with_ymd_and_hms(dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(), 0)
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad minute".into()))?,
        "second" => Utc
            .with_ymd_and_hms(
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second(),
            )
            .single()
            .ok_or_else(|| SQLError::TypeMismatch("date_trunc: bad second".into()))?,
        other => {
            return Err(SQLError::Unsupported(format!("date_trunc unit `{other}`")));
        }
    };
    Ok(Value::Str(
        truncated.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
    ))
}

pub(super) fn make_timestamp(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: f64,
) -> Result<Value> {
    use chrono::{NaiveDate, TimeZone, Utc};
    let secs = second.trunc() as u32;
    let nanos = (second.fract() * 1e9).round() as u32;
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| SQLError::TypeMismatch("make_timestamp: bad date".into()))?;
    let naive = date
        .and_hms_nano_opt(hour, minute, secs, nanos)
        .ok_or_else(|| SQLError::TypeMismatch("make_timestamp: bad time".into()))?;
    let dt = Utc.from_utc_datetime(&naive);
    Ok(Value::Str(
        dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
    ))
}

pub(super) fn pg_to_chrono_fmt(fmt: &str) -> String {
    // Translate a small subset of PostgreSQL `to_date` template tokens
    // into chrono format specifiers. The canonical UQA behavior relies on
    // datetime.strptime; this routine mirrors the most common patterns
    // (`YYYY`, `MM`, `DD`, `HH24`, `MI`, `SS`).
    fmt.replace("YYYY", "%Y")
        .replace("YY", "%y")
        .replace("MM", "%m")
        .replace("DD", "%d")
        .replace("HH24", "%H")
        .replace("HH12", "%I")
        .replace("MI", "%M")
        .replace("SS", "%S")
}

pub(super) fn format_pg_number(n: f64, fmt: &str) -> String {
    // Minimal `to_char(numeric, '999...')` support: count `9` digits
    // and zero-pad the integral part. Falls back to plain Display.
    let digits = fmt.chars().filter(|c| *c == '9' || *c == '0').count();
    if digits == 0 {
        return n.to_string();
    }
    let int_part = n.trunc() as i64;
    if fmt.contains('.') {
        let frac_digits = fmt.split('.').nth(1).map(str::len).unwrap_or(0);
        format!("{n:.frac_digits$}")
    } else {
        format!("{int_part:0digits$}")
    }
}

pub(super) fn format_pg_datetime(dt: &chrono::DateTime<chrono::Utc>, fmt: &str) -> String {
    dt.format(&pg_to_chrono_fmt(fmt)).to_string()
}

pub(super) fn generate_random_uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&now_ns.to_be_bytes());
    bytes[8..].copy_from_slice(&counter.to_be_bytes());
    // Set version 4 + variant per RFC 4122.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parse a timestamp string into a UTC `DateTime`. Accepts:
/// - RFC3339 (`2025-01-31T12:00:00Z`, `2025-01-31T12:00:00+09:00`)
/// - PostgreSQL-ish `YYYY-MM-DD HH:MM:SS[.fff]` (assumed UTC)
/// - Bare date `YYYY-MM-DD` (midnight UTC).
pub(super) fn parse_timestamp(s: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    let formats: &[&str] = &[
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ];
    for fmt in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(Utc.from_utc_datetime(&naive));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let naive = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| SQLError::TypeMismatch(format!("bad date {s}")))?;
        return Ok(Utc.from_utc_datetime(&naive));
    }
    Err(SQLError::TypeMismatch(format!(
        "cannot parse timestamp {s:?}"
    )))
}

/// `EXTRACT(field FROM ts)` field selectors. Matches UQA
/// reference's `_sf_extract` implementation.
pub(super) fn extract_field(field: &str, dt: &chrono::DateTime<chrono::Utc>) -> Result<Value> {
    use chrono::{Datelike, Timelike};
    match field {
        "year" => Ok(Value::Int(i64::from(dt.year()))),
        "month" => Ok(Value::Int(i64::from(dt.month()))),
        "day" => Ok(Value::Int(i64::from(dt.day()))),
        "hour" => Ok(Value::Int(i64::from(dt.hour()))),
        "minute" => Ok(Value::Int(i64::from(dt.minute()))),
        "second" => {
            let secs = f64::from(dt.second()) + f64::from(dt.nanosecond()) / 1_000_000_000.0;
            Ok(Value::Float(secs))
        }
        "millisecond" => Ok(Value::Int(i64::from(dt.timestamp_subsec_millis()))),
        "microsecond" => Ok(Value::Int(i64::from(dt.timestamp_subsec_micros()))),
        "dow" => Ok(Value::Int(i64::from(dt.weekday().num_days_from_sunday()))),
        "isodow" => Ok(Value::Int(i64::from(dt.weekday().number_from_monday()))),
        "doy" => Ok(Value::Int(i64::from(dt.ordinal()))),
        "epoch" => Ok(Value::Float(
            dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_micros()) / 1_000_000.0,
        )),
        "quarter" => Ok(Value::Int(i64::from(dt.month() - 1) / 3 + 1)),
        "week" => Ok(Value::Int(i64::from(dt.iso_week().week()))),
        other => Err(SQLError::Unsupported(format!("EXTRACT field `{other}`"))),
    }
}
