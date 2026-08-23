//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible UUID parsing, generation, and extraction.

use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};

use uqa_core::{TemporalValue, Value};

use crate::error::{Result, SQLError};

use super::{out_of_range, time::timestamp_plus_interval};

const NANOS_PER_MICROSECOND: i64 = 1_000;
const NANOS_PER_MILLISECOND: i64 = 1_000_000;
const UUID_V1_UNIX_EPOCH_OFFSET_TICKS: i128 = 0x01b2_1dd2_1381_4000;
const UUID_V7_SUBMILLISECOND_BITS: u32 = 12;
const UUID_V7_MAX_UNIX_MILLISECONDS: i64 = 0x0000_ffff_ffff_ffff;

#[cfg(any(target_os = "macos", target_os = "windows"))]
const UUID_V7_CLOCK_PRECISION_BITS: u32 = 10;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const UUID_V7_CLOCK_PRECISION_BITS: u32 = 12;

const UUID_V7_MINIMUM_STEP_NANOS: i64 =
    NANOS_PER_MILLISECOND / (1_i64 << UUID_V7_CLOCK_PRECISION_BITS) + 1;
static UUID_V7_PREVIOUS_NANOS: AtomicI64 = AtomicI64::new(0);

pub(super) fn canonicalize_uuid(text: &str) -> Result<String> {
    parse_uuid_bytes(text).map(format_uuid)
}

pub(super) fn extract_uuid_version(value: &Value) -> Result<Value> {
    let bytes = uuid_value_bytes(value)?;
    Ok(uuid_version(&bytes).map_or(Value::Null, |version| Value::Int(i64::from(version))))
}

pub(super) fn extract_uuid_timestamp(value: &Value) -> Result<Value> {
    let bytes = uuid_value_bytes(value)?;
    let Some(version) = uuid_version(&bytes) else {
        return Ok(Value::Null);
    };
    let micros = match version {
        1 => uuid_v1_unix_micros(&bytes),
        7 => uuid_v7_unix_micros(&bytes),
        _ => return Ok(Value::Null),
    }?;
    Ok(Value::Temporal(TemporalValue::TimestampTz { micros }))
}

pub(super) fn generate_random_uuid() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| SQLError::Internal(format!("failed to obtain random bytes: {error}")))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format_uuid(bytes))
}

pub(super) fn generate_uuid_v7(shift: Option<&TemporalValue>) -> Result<String> {
    let now_nanos = real_time_nanos_ascending()?;
    let now_micros = now_nanos.div_euclid(NANOS_PER_MICROSECOND);
    let sub_microsecond_nanos = now_nanos.rem_euclid(NANOS_PER_MICROSECOND);
    let timestamp_micros = match shift {
        None => now_micros,
        Some(TemporalValue::Interval {
            months,
            days,
            micros,
        }) => timestamp_plus_interval(now_micros, *months, *days, *micros)?,
        Some(other) => {
            return Err(SQLError::TypeMismatch(format!(
                "uuidv7: expected interval, got {other:?}"
            )));
        }
    };
    let unix_millis = timestamp_micros.div_euclid(1_000);
    if !(0..=UUID_V7_MAX_UNIX_MILLISECONDS).contains(&unix_millis) {
        return Err(out_of_range("uuidv7 timestamp"));
    }
    let sub_millisecond_nanos = timestamp_micros
        .rem_euclid(1_000)
        .checked_mul(NANOS_PER_MICROSECOND)
        .and_then(|nanos| nanos.checked_add(sub_microsecond_nanos))
        .ok_or_else(|| out_of_range("uuidv7 timestamp"))?;
    let sub_millisecond_nanos =
        u32::try_from(sub_millisecond_nanos).map_err(|_| out_of_range("uuidv7 timestamp"))?;
    generate_uuid_v7_at(unix_millis as u64, sub_millisecond_nanos)
}

fn parse_uuid_bytes(text: &str) -> Result<[u8; 16]> {
    let digits = text
        .strip_prefix('{')
        .and_then(|text| text.strip_suffix('}'))
        .unwrap_or(text);
    if digits.starts_with('{') || digits.ends_with('}') {
        return Err(invalid_uuid(text));
    }
    let mut normalized = String::with_capacity(32);
    let mut group_digits = 0_usize;
    for character in digits.chars() {
        if character == '-' {
            if group_digits == 0 || !group_digits.is_multiple_of(4) {
                return Err(invalid_uuid(text));
            }
            group_digits = 0;
            continue;
        }
        if !character.is_ascii_hexdigit() {
            return Err(invalid_uuid(text));
        }
        normalized.push(character.to_ascii_lowercase());
        group_digits += 1;
    }
    if normalized.len() != 32 || group_digits == 0 {
        return Err(invalid_uuid(text));
    }
    let mut bytes = [0_u8; 16];
    for (index, pair) in normalized.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_value(pair[0]) << 4) | hex_value(pair[1]);
    }
    Ok(bytes)
}

fn uuid_value_bytes(value: &Value) -> Result<[u8; 16]> {
    match value {
        Value::Str(text) | Value::FixedChar(text) => parse_uuid_bytes(text),
        other => Err(SQLError::TypeMismatch(format!(
            "expected uuid value, got {other:?}"
        ))),
    }
}

fn uuid_version(bytes: &[u8; 16]) -> Option<u8> {
    ((bytes[8] & 0xc0) == 0x80).then_some(bytes[6] >> 4)
}

fn uuid_v1_unix_micros(bytes: &[u8; 16]) -> Result<i64> {
    let low = u32::from_be_bytes(bytes[0..4].try_into().expect("UUID time_low width"));
    let middle = u16::from_be_bytes(bytes[4..6].try_into().expect("UUID time_mid width"));
    let high = u16::from_be_bytes(bytes[6..8].try_into().expect("UUID time_high width")) & 0x0fff;
    let ticks = (i128::from(high) << 48) | (i128::from(middle) << 32) | i128::from(low);
    i64::try_from((ticks - UUID_V1_UNIX_EPOCH_OFFSET_TICKS).div_euclid(10))
        .map_err(|_| out_of_range("uuid timestamp"))
}

fn uuid_v7_unix_micros(bytes: &[u8; 16]) -> Result<i64> {
    let milliseconds = bytes[..6]
        .iter()
        .fold(0_i64, |value, byte| (value << 8) | i64::from(*byte));
    milliseconds
        .checked_mul(1_000)
        .ok_or_else(|| out_of_range("uuid timestamp"))
}

fn real_time_nanos_ascending() -> Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| out_of_range("uuidv7 timestamp"))?;
    let actual = i64::try_from(elapsed.as_nanos()).map_err(|_| out_of_range("uuidv7 timestamp"))?;
    loop {
        let previous = UUID_V7_PREVIOUS_NANOS.load(AtomicOrdering::Relaxed);
        let minimum = previous
            .checked_add(UUID_V7_MINIMUM_STEP_NANOS)
            .ok_or_else(|| out_of_range("uuidv7 timestamp"))?;
        let candidate = if minimum >= actual { minimum } else { actual };
        if UUID_V7_PREVIOUS_NANOS
            .compare_exchange_weak(
                previous,
                candidate,
                AtomicOrdering::Relaxed,
                AtomicOrdering::Relaxed,
            )
            .is_ok()
        {
            return Ok(candidate);
        }
    }
}

fn generate_uuid_v7_at(unix_millis: u64, sub_millisecond_nanos: u32) -> Result<String> {
    if unix_millis > UUID_V7_MAX_UNIX_MILLISECONDS as u64
        || sub_millisecond_nanos >= NANOS_PER_MILLISECOND as u32
    {
        return Err(out_of_range("uuidv7 timestamp"));
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes[8..])
        .map_err(|error| SQLError::Internal(format!("failed to obtain random bytes: {error}")))?;
    let timestamp = unix_millis.to_be_bytes();
    bytes[..6].copy_from_slice(&timestamp[2..]);
    let increased_clock_precision = (u64::from(sub_millisecond_nanos)
        * (1_u64 << UUID_V7_SUBMILLISECOND_BITS))
        / NANOS_PER_MILLISECOND as u64;
    bytes[6] = (increased_clock_precision >> 8) as u8;
    bytes[7] = increased_clock_precision as u8;

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        bytes[7] ^= bytes[8] >> 6;
    }

    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format_uuid(bytes))
}

fn format_uuid(bytes: [u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("UUID parser retained only lowercase hexadecimal digits"),
    }
}

fn invalid_uuid(text: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22P02".into(),
        message: format!("invalid input syntax for type uuid: \"{text}\""),
    }
}
