//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked finite temporal construction at host-language boundaries.

use super::{TemporalValue, MICROS_PER_DAY, POSTGRES_EPOCH_DAYS};

impl TemporalValue {
    /// Construct an admitted finite DATE from `PostgreSQL` days since 2000-01-01.
    #[must_use]
    pub fn from_postgres_date(days: i32) -> Option<Self> {
        let days = days.checked_add(POSTGRES_EPOCH_DAYS)?;
        Self::date_days_in_range(days).then_some(Self::Date { days })
    }

    /// Construct finite wall-clock time, including the distinct 24:00:00 endpoint.
    #[must_use]
    pub fn from_time_micros(micros: i64) -> Option<Self> {
        (0..=MICROS_PER_DAY)
            .contains(&micros)
            .then_some(Self::Time { micros })
    }

    /// Construct time with seconds east of UTC, including the complete ±15:59:59 zone range.
    #[must_use]
    pub fn from_time_tz_micros(micros: i64, offset_seconds: i32) -> Option<Self> {
        Self::from_time_micros(micros)?;
        (-57_599..=57_599)
            .contains(&offset_seconds)
            .then_some(Self::TimeTz {
                micros,
                offset_seconds,
            })
    }

    /// Construct a finite timestamp in the complete `PostgreSQL` microsecond domain.
    #[must_use]
    pub fn from_postgres_timestamp(postgres_micros: i64, with_time_zone: bool) -> Option<Self> {
        Self::timestamp_micros_in_range(postgres_micros).then_some(if with_time_zone {
            Self::TimestampTz { postgres_micros }
        } else {
            Self::Timestamp { postgres_micros }
        })
    }

    /// Construct a finite interval without normalizing months, days or microseconds; reserved infinite tuples are not finite input.
    #[must_use]
    pub fn from_interval_fields(months: i32, days: i32, micros: i64) -> Option<Self> {
        let value = Self::Interval {
            months,
            days,
            micros,
        };
        value.is_finite().then_some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_carriers_cover_sql_endpoints_and_reject_outside_values() {
        for (days, text) in [
            (-2_451_545, "4714-11-24 BC"),
            (0, "2000-01-01"),
            (2_145_031_948, "5874897-12-31"),
        ] {
            assert_eq!(
                TemporalValue::from_postgres_date(days)
                    .unwrap()
                    .to_sql_string(),
                text
            );
        }
        for days in [i32::MIN, -2_451_546, 2_145_031_949, i32::MAX] {
            assert!(TemporalValue::from_postgres_date(days).is_none());
        }
        for micros in [-211_813_488_000_000_000, 0, 9_223_371_331_199_999_999] {
            for zoned in [false, true] {
                assert!(TemporalValue::from_postgres_timestamp(micros, zoned).is_some());
            }
        }
        for micros in [
            i64::MIN,
            -211_813_488_000_000_001,
            9_223_371_331_200_000_000,
            i64::MAX,
        ] {
            assert!(TemporalValue::from_postgres_timestamp(micros, false).is_none());
        }
        assert!(TemporalValue::from_time_micros(-1).is_none());
        assert!(TemporalValue::from_time_micros(MICROS_PER_DAY + 1).is_none());
        for offset in [-57_599, 0, 57_599] {
            assert!(TemporalValue::from_time_tz_micros(MICROS_PER_DAY, offset).is_some());
        }
        for offset in [-57_600, 57_600] {
            assert!(TemporalValue::from_time_tz_micros(0, offset).is_none());
        }
        assert!(TemporalValue::from_interval_fields(i32::MAX, i32::MAX, i64::MAX).is_none());
        assert!(TemporalValue::from_interval_fields(i32::MAX, i32::MAX, i64::MAX - 1).is_some());
    }
}
