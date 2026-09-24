//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Convert `PostgreSQL`'s 2000 epoch Datums to shared temporal values.

use crate::catalog::node_tree::invalid;
use crate::SQLError;
use uqa_core::TemporalValue;

const EPOCH_DAYS: i32 = 10_957;
const EPOCH_MICROS: i64 = 946_684_800_000_000;

pub(super) fn encode(value: &TemporalValue, oid: i64) -> Result<Vec<u8>, SQLError> {
    let bytes = match (value, oid) {
        (TemporalValue::Date { days }, 1082) => {
            let days = days
                .checked_sub(EPOCH_DAYS)
                .ok_or_else(|| invalid("date Datum overflow"))?;
            i64::from(days).to_le_bytes().to_vec()
        }
        (TemporalValue::Time { micros }, 1083) => micros.to_le_bytes().to_vec(),
        (TemporalValue::Timestamp { micros }, 1114)
        | (TemporalValue::TimestampTz { micros }, 1184) => micros
            .checked_sub(EPOCH_MICROS)
            .ok_or_else(|| invalid("timestamp Datum overflow"))?
            .to_le_bytes()
            .to_vec(),
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
            micros
                .to_le_bytes()
                .into_iter()
                .chain(offset.to_le_bytes())
                .collect()
        }
        (
            TemporalValue::Interval {
                months,
                days,
                micros,
            },
            1186,
        ) => micros
            .to_le_bytes()
            .into_iter()
            .chain(days.to_le_bytes())
            .chain(months.to_le_bytes())
            .collect(),
        _ => return Err(invalid("temporal Datum differs from its declared type")),
    };
    Ok(bytes)
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
        1082 => TemporalValue::Date {
            days: integer(0)?
                .checked_add(EPOCH_DAYS)
                .ok_or_else(|| invalid("date Datum overflow"))?,
        },
        1083 => TemporalValue::Time { micros: micros()? },
        1114 | 1184 => {
            let micros = micros()?
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
