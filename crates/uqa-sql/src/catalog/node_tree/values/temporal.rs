//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Convert `PostgreSQL`'s 2000 epoch Datums to shared temporal values.

use crate::catalog::node_tree::invalid;
use crate::SQLError;
use uqa_core::{TemporalInfinity, TemporalValue};

const EPOCH_DAYS: i32 = 10_957;
const EPOCH_MICROS: i64 = 946_684_800_000_000;

pub(crate) fn encode(value: &TemporalValue, oid: i64) -> Result<Vec<u8>, SQLError> {
    let (bytes, length) = encode_inline(value, oid)?;
    Ok(bytes[..length].to_vec())
}

/// Temporal Datums fit in sixteen bytes; callers can admit their destination before copying without allocating scalar scratch.
pub(crate) fn encode_inline(
    value: &TemporalValue,
    oid: i64,
) -> Result<([u8; 16], usize), SQLError> {
    let mut bytes = [0; 16];
    let length = match (value, oid) {
        (TemporalValue::Date { days }, 1082) => {
            let days = if value.is_finite() {
                days.checked_sub(EPOCH_DAYS)
                    .ok_or_else(|| invalid("date Datum overflow"))?
            } else {
                *days
            };
            bytes[..8].copy_from_slice(&i64::from(days).to_le_bytes());
            8
        }
        (TemporalValue::Time { micros }, 1083) => {
            bytes[..8].copy_from_slice(&micros.to_le_bytes());
            8
        }
        (TemporalValue::Timestamp { micros }, 1114)
        | (TemporalValue::TimestampTz { micros }, 1184) => {
            let micros = match value.infinity() {
                Some(TemporalInfinity::Negative) => i64::MIN,
                Some(TemporalInfinity::Positive) => i64::MAX,
                None => micros
                    .checked_sub(EPOCH_MICROS)
                    .ok_or_else(|| invalid("timestamp Datum overflow"))?,
            };
            bytes[..8].copy_from_slice(&micros.to_le_bytes());
            8
        }
        (
            TemporalValue::TimeTz {
                micros,
                offset_minutes,
            },
            1266,
        ) => {
            let offset = offset_minutes
                .checked_mul(-60)
                .ok_or_else(|| invalid("time zone Datum overflow"))?;
            bytes[..8].copy_from_slice(&micros.to_le_bytes());
            bytes[8..12].copy_from_slice(&offset.to_le_bytes());
            12
        }
        (
            TemporalValue::Interval {
                months,
                days,
                micros,
            },
            1186,
        ) => {
            bytes[..8].copy_from_slice(&micros.to_le_bytes());
            bytes[8..12].copy_from_slice(&days.to_le_bytes());
            bytes[12..].copy_from_slice(&months.to_le_bytes());
            16
        }
        _ => return Err(invalid("temporal Datum differs from its declared type")),
    };
    Ok((bytes, length))
}

pub(crate) fn decode(bytes: &[u8], oid: i64) -> Result<TemporalValue, SQLError> {
    let integer = |offset: usize| -> Result<i32, SQLError> {
        bytes
            .get(offset..offset + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(i32::from_le_bytes)
            .ok_or_else(|| invalid("truncated temporal Datum"))
    };
    let micros = || -> Result<i64, SQLError> {
        bytes
            .get(..8)
            .and_then(|bytes| bytes.try_into().ok())
            .map(i64::from_le_bytes)
            .ok_or_else(|| invalid("truncated temporal Datum"))
    };
    Ok(match oid {
        1082 => {
            let value = TemporalValue::Date { days: integer(0)? };
            if !value.is_finite() {
                return Ok(value);
            }
            TemporalValue::Date {
                days: integer(0)?
                    .checked_add(EPOCH_DAYS)
                    .ok_or_else(|| invalid("date Datum overflow"))?,
            }
        }
        1083 => TemporalValue::Time { micros: micros()? },
        1114 | 1184 => {
            let micros = micros()?;
            let infinity = match micros {
                i64::MIN => Some(TemporalInfinity::Negative),
                i64::MAX => Some(TemporalInfinity::Positive),
                _ => None,
            };
            if let Some(infinity) = infinity {
                return Ok(if oid == 1114 {
                    infinity.timestamp()
                } else {
                    infinity.timestamp_tz()
                });
            }
            let micros = micros
                .checked_add(EPOCH_MICROS)
                .ok_or_else(|| invalid("timestamp Datum overflow"))?;
            if oid == 1114 {
                TemporalValue::Timestamp { micros }
            } else {
                TemporalValue::TimestampTz { micros }
            }
        }
        1266 => {
            let seconds = integer(8)?;
            if seconds % 60 != 0 {
                return Err(invalid("time zone offset is not a whole minute"));
            }
            TemporalValue::TimeTz {
                micros: micros()?,
                offset_minutes: seconds / -60,
            }
        }
        1186 => TemporalValue::Interval {
            months: integer(12)?,
            days: integer(8)?,
            micros: micros()?,
        },
        _ => return Err(invalid("unknown temporal Datum type")),
    })
}
