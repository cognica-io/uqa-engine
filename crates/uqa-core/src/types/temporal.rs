//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL temporal values, parsing, formatting, and total ordering.

use super::Ordering;
#[cfg(test)]
use super::{DateTime, NaiveDate};
use crate::{
    memory::{ProductionControl, ProductionString},
    ValueRetentionError,
};

mod infinite;
mod input;
mod interval_input;
mod interval_style;
mod keys;
mod production;
mod timezone;

pub use infinite::TemporalInfinity;
mod calendar;
pub use input::{TemporalDateOrder, TemporalInputError};
pub use interval_style::TemporalIntervalStyle;
pub use timezone::TemporalTimeZone;

pub(super) const MICROS_PER_SECOND: i64 = 1_000_000;
pub(super) const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;

/// Compact temporal values used by SQL `DATE`, `TIME`, and `TIMESTAMP`
/// columns. The payload is numeric so comparison and sorting do not
/// depend on string collation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "$uqa_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TemporalValue {
    Date {
        days: i32,
    },
    Time {
        micros: i64,
    },
    TimeTz {
        micros: i64,
        offset_minutes: i32,
    },
    Timestamp {
        micros: i64,
    },
    TimestampTz {
        micros: i64,
    },
    /// `INTERVAL` values use `PostgreSQL`'s exact three-field model:
    /// months and days stay symbolic (a month is not a fixed number of
    /// days) while sub-day amounts collapse into microseconds.
    Interval {
        months: i32,
        days: i32,
        micros: i64,
    },
}

impl TemporalValue {
    /// Test a finite Unix-day date against `PostgreSQL`'s Julian range, excluding infinity sentinels.
    #[must_use]
    pub fn date_days_in_range(days: i32) -> bool {
        calendar::valid_date(days)
    }

    /// Test a finite Unix-microsecond timestamp against `PostgreSQL`'s Julian range. The epoch conversion uses a wider integer because `PostgreSQL`'s upper bound lies beyond this carrier's largest Unix-microsecond value.
    #[must_use]
    pub const fn timestamp_micros_in_range(micros: i64) -> bool {
        let postgres_micros = micros as i128 - 946_684_800_000_000;
        postgres_micros >= -211_813_488_000_000_000 && postgres_micros < 9_223_371_331_200_000_000
    }

    /// The `parse_*` readers are the input functions with the wall clock standing in for the transaction start that `now` and `today` name; a text the type rejects reads as `None`.
    pub fn parse_date(input: &str) -> Option<Self> {
        Self::date_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_time(input: &str) -> Option<Self> {
        Self::time_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_time_tz(input: &str) -> Option<Self> {
        Self::time_tz_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_timestamp(input: &str) -> Option<Self> {
        Self::timestamp_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_timestamp_tz(input: &str) -> Option<Self> {
        Self::timestamp_tz_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_same_kind(&self, input: &str) -> Option<Self> {
        self.parse_same_kind_with_control(input, &ProductionControl::uncontrolled())
            .expect("ordinary temporal kind parser")
    }

    /// Parse a `PostgreSQL` interval literal (`'1 day'`, `'90 minutes'`,
    /// `'1 day 3 hours'`, `'1-2'`, `'3 4:05:06'`, bare seconds, `ago`).
    /// Fractional quantities cascade into the next-smaller unit exactly
    /// like `PostgreSQL` (`'1.5 mons'` -> `1 mon 15 days`).
    pub fn parse_interval(input: &str) -> Option<Self> {
        Self::parse_interval_with_control(input, &ProductionControl::uncontrolled())
            .ok()
            .flatten()
    }

    pub fn to_sql_string(&self) -> String {
        self.to_sql_string_with_control(&ProductionControl::uncontrolled())
            .expect("ordinary temporal formatting")
            .into_uncontrolled()
            .expect("ordinary temporal text")
    }

    fn sort_key(&self) -> (u8, i128, i64) {
        match self {
            Self::Date { days } => (0, i128::from(*days), 0),
            Self::Time { micros } => (1, i128::from(*micros), 0),
            Self::TimeTz {
                micros,
                offset_minutes,
            } => (
                2,
                i128::from(*micros)
                    - i128::from(*offset_minutes) * 60 * i128::from(MICROS_PER_SECOND),
                // PostgreSQL compares seconds west of UTC after adjusted time; our carrier stores minutes east. Widen before negation for arbitrary deserialized carriers.
                -i64::from(*offset_minutes),
            ),
            Self::Timestamp { micros } => (3, self.timestamp_rank(*micros), 0),
            Self::TimestampTz { micros } => (4, self.timestamp_rank(*micros), 0),
            // PostgreSQL's interval_cmp flattens to microseconds with
            // 30-day months for ordering purposes.
            Self::Interval {
                months,
                days,
                micros,
            } => (
                5,
                (i128::from(*months) * 30 + i128::from(*days)) * i128::from(MICROS_PER_DAY)
                    + i128::from(*micros),
                0,
            ),
        }
    }
}

impl PartialEq for TemporalValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for TemporalValue {}

impl PartialOrd for TemporalValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TemporalValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

#[cfg(test)]
fn epoch_date() -> NaiveDate {
    DateTime::<chrono::Utc>::UNIX_EPOCH.date_naive()
}

/// The platform clock in Unix microseconds, which stands in for the transaction start when no statement supplies one.
fn wall_clock_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}
