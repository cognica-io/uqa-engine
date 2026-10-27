//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal wire tags distinguish epochs and zone units while retaining predecessor readers.

use super::{TemporalValue, POSTGRES_EPOCH_MICROS};
use serde::{Deserialize, Deserializer};

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(tag = "$uqa_type", rename_all = "snake_case", deny_unknown_fields)]
enum EncodedTemporal {
    Date { days: i32 },
    Time { micros: i64 },
    TimeTz { micros: i64, offset_minutes: i32 },
    TimeTzSeconds { micros: i64, offset_seconds: i32 },
    Timestamp { micros: i64 },
    TimestampTz { micros: i64 },
    PostgresTimestamp { postgres_micros: i64 },
    PostgresTimestampTz { postgres_micros: i64 },
    Interval { months: i32, days: i32, micros: i64 },
}

impl<'de> Deserialize<'de> for TemporalValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let overflow = || serde::de::Error::custom("legacy temporal carrier overflow");
        Ok(match EncodedTemporal::deserialize(deserializer)? {
            EncodedTemporal::Date { days } => Self::Date { days },
            EncodedTemporal::Time { micros } => Self::Time { micros },
            EncodedTemporal::TimeTz {
                micros,
                offset_minutes,
            } => Self::TimeTz {
                micros,
                offset_seconds: offset_minutes.checked_mul(60).ok_or_else(overflow)?,
            },
            EncodedTemporal::TimeTzSeconds {
                micros,
                offset_seconds,
            } => Self::TimeTz {
                micros,
                offset_seconds,
            },
            EncodedTemporal::Timestamp { micros } => Self::Timestamp {
                postgres_micros: legacy_timestamp(micros).ok_or_else(overflow)?,
            },
            EncodedTemporal::TimestampTz { micros } => Self::TimestampTz {
                postgres_micros: legacy_timestamp(micros).ok_or_else(overflow)?,
            },
            EncodedTemporal::PostgresTimestamp { postgres_micros } => {
                Self::Timestamp { postgres_micros }
            }
            EncodedTemporal::PostgresTimestampTz { postgres_micros } => {
                Self::TimestampTz { postgres_micros }
            }
            EncodedTemporal::Interval {
                months,
                days,
                micros,
            } => Self::Interval {
                months,
                days,
                micros,
            },
        })
    }
}

/// Predecessor Unix carriers used two lower sentinels, leaving `i64::MAX` finite.
pub(crate) fn legacy_timestamp(micros: i64) -> Option<i64> {
    match micros {
        i64::MIN => Some(i64::MIN),
        value if value == i64::MIN + 1 => Some(i64::MAX),
        value => value
            .checked_sub(POSTGRES_EPOCH_MICROS)
            .filter(|value| !matches!(*value, i64::MIN | i64::MAX)),
    }
}
