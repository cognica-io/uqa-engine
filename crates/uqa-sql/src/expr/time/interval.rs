//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Interval arithmetic as `PostgreSQL` 18's `timestamp.c` performs it on finite intervals: negation (`interval_um_internal`), field-wise addition and subtraction (`finite_interval_pl`, `finite_interval_mi`), and scaling by a double precision factor (`interval_mul`, `interval_div`). Every function reports overflow as `interval out of range` (SQLSTATE 22008), including a result that lands on the field values reserved for `-infinity` and `infinity`.

use uqa_core::TemporalValue;

use super::super::{datetime_out_of_range, division_by_zero};
use crate::error::Result;

/// The months, days and microseconds of an interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntervalFields {
    pub months: i32,
    pub days: i32,
    pub micros: i64,
}

const DAYS_PER_MONTH_INT: i32 = 30;
const DAYS_PER_MONTH: f64 = 30.0;
const SECS_PER_DAY: f64 = 86_400.0;
const USECS_PER_SEC: f64 = 1_000_000.0;
const USECS_PER_DAY: i64 = 86_400_000_000;

impl IntervalFields {
    pub const ZERO: Self = Self {
        months: 0,
        days: 0,
        micros: 0,
    };

    /// The fields of an interval value, or `None` for any other temporal value.
    pub fn of(value: &TemporalValue) -> Option<Self> {
        match *value {
            TemporalValue::Interval {
                months,
                days,
                micros,
            } => Some(Self {
                months,
                days,
                micros,
            }),
            _ => None,
        }
    }

    pub fn value(self) -> TemporalValue {
        TemporalValue::Interval {
            months: self.months,
            days: self.days,
            micros: self.micros,
        }
    }

    /// `INTERVAL_NOT_FINITE`: the field values `interval '-infinity'` and `interval 'infinity'` are stored as.
    fn is_reserved(self) -> bool {
        (self.months == i32::MIN && self.days == i32::MIN && self.micros == i64::MIN)
            || (self.months == i32::MAX && self.days == i32::MAX && self.micros == i64::MAX)
    }

    fn finite(self) -> Result<Self> {
        if self.is_reserved() {
            Err(datetime_out_of_range("interval"))
        } else {
            Ok(self)
        }
    }

    /// `interval_um_internal`.
    pub fn negate(self) -> Result<Self> {
        match (
            self.months.checked_neg(),
            self.days.checked_neg(),
            self.micros.checked_neg(),
        ) {
            (Some(months), Some(days), Some(micros)) => Self {
                months,
                days,
                micros,
            }
            .finite(),
            _ => Err(datetime_out_of_range("interval")),
        }
    }

    /// `finite_interval_pl`.
    pub fn plus(self, other: Self) -> Result<Self> {
        match (
            self.months.checked_add(other.months),
            self.days.checked_add(other.days),
            self.micros.checked_add(other.micros),
        ) {
            (Some(months), Some(days), Some(micros)) => Self {
                months,
                days,
                micros,
            }
            .finite(),
            _ => Err(datetime_out_of_range("interval")),
        }
    }

    /// `finite_interval_mi`.
    pub fn minus(self, other: Self) -> Result<Self> {
        match (
            self.months.checked_sub(other.months),
            self.days.checked_sub(other.days),
            self.micros.checked_sub(other.micros),
        ) {
            (Some(months), Some(days), Some(micros)) => Self {
                months,
                days,
                micros,
            }
            .finite(),
            _ => Err(datetime_out_of_range("interval")),
        }
    }

    /// `interval_mul` of a finite interval.
    pub fn multiply(self, factor: f64) -> Result<Self> {
        if factor.is_nan() || factor.is_infinite() {
            return Err(datetime_out_of_range("interval"));
        }
        self.scale(|field| field * factor)
    }

    /// `interval_div` of a finite interval.
    pub fn divide(self, factor: f64) -> Result<Self> {
        if factor == 0.0 {
            return Err(division_by_zero());
        }
        if factor.is_nan() {
            return Err(datetime_out_of_range("interval"));
        }
        self.scale(|field| field / factor)
    }

    /// `interval_justify_hours`: whole days of the time field move into the day field, then a day and a time of opposite signs trade one day so that both take one sign.
    pub fn justify_hours(self) -> Result<Self> {
        let whole_days = self.micros / USECS_PER_DAY;
        let days = self
            .days
            .checked_add(whole_days as i32)
            .ok_or_else(|| datetime_out_of_range("interval"))?;
        let (days, micros) = align_time_with_days(days, self.micros - whole_days * USECS_PER_DAY);
        Ok(Self {
            months: self.months,
            days,
            micros,
        })
    }

    /// `interval_justify_days`: whole 30-day months of the day field move into the month field, then a month and a day of opposite signs trade one month.
    pub fn justify_days(self) -> Result<Self> {
        let (months, days) = carry_whole_months(self.months, self.days)?;
        let (months, days) = if months > 0 && days < 0 {
            (months - 1, days + DAYS_PER_MONTH_INT)
        } else if months < 0 && days > 0 {
            (months + 1, days - DAYS_PER_MONTH_INT)
        } else {
            (months, days)
        };
        Ok(Self {
            months,
            days,
            micros: self.micros,
        })
    }

    /// `interval_justify_interval`: `justify_hours` and `justify_days` together, so that all three fields take one sign. Days are carried into months first when day and time share a sign, which keeps the day field from overflowing.
    pub fn justify_interval(self) -> Result<Self> {
        let (mut months, mut days) = (self.months, self.days);
        if (days > 0 && self.micros > 0) || (days < 0 && self.micros < 0) {
            (months, days) = carry_whole_months(months, days)?;
        }
        let whole_days = self.micros / USECS_PER_DAY;
        let mut micros = self.micros - whole_days * USECS_PER_DAY;
        // Either the pre-carry left fewer than 30 days or day and time differ in sign, so this cannot overflow.
        days = i32::try_from(i64::from(days) + whole_days)
            .map_err(|_| datetime_out_of_range("interval"))?;
        (months, days) = carry_whole_months(months, days)?;
        if months > 0 && (days < 0 || (days == 0 && micros < 0)) {
            days += DAYS_PER_MONTH_INT;
            months -= 1;
        } else if months < 0 && (days > 0 || (days == 0 && micros > 0)) {
            days -= DAYS_PER_MONTH_INT;
            months += 1;
        }
        (days, micros) = align_time_with_days(days, micros);
        Ok(Self {
            months,
            days,
            micros,
        })
    }

    /// The body `interval_mul` and `interval_div` share: each field is scaled on its own and truncated, the fractional month cascades into days at 30 days a month and the fractional day into seconds at 86400 seconds a day, and nothing cascades upward. `TSROUND` keeps the cascaded fractions from drifting off whole microseconds.
    fn scale(self, scale: impl Fn(f64) -> f64) -> Result<Self> {
        let months = truncate_to_i32(scale(f64::from(self.months)))?;
        let days = truncate_to_i32(scale(f64::from(self.days)))?;
        let month_remainder_days =
            timestamp_round((scale(f64::from(self.months)) - f64::from(months)) * DAYS_PER_MONTH);
        let mut sec_remainder = timestamp_round(
            (scale(f64::from(self.days)) - f64::from(days) + month_remainder_days
                - f64::from(month_remainder_days as i32))
                * SECS_PER_DAY,
        );
        let mut days = days;
        // Rounding can leave a whole day of seconds, and the cascades together can exceed one.
        if sec_remainder.abs() >= SECS_PER_DAY {
            let whole_days = (sec_remainder / SECS_PER_DAY) as i32;
            days = days
                .checked_add(whole_days)
                .ok_or_else(|| datetime_out_of_range("interval"))?;
            sec_remainder -= f64::from(whole_days) * SECS_PER_DAY;
        }
        let days = days
            .checked_add(month_remainder_days as i32)
            .ok_or_else(|| datetime_out_of_range("interval"))?;
        let micros = (scale(self.micros as f64) + sec_remainder * USECS_PER_SEC).round_ties_even();
        if micros.is_nan() || !(micros >= i64::MIN as f64 && micros < -(i64::MIN as f64)) {
            return Err(datetime_out_of_range("interval"));
        }
        Self {
            months,
            days,
            micros: micros as i64,
        }
        .finite()
    }
}

/// Move the whole 30-day months of `days` into `months`, the day count's sign kept as C's truncating division keeps it.
fn carry_whole_months(months: i32, days: i32) -> Result<(i32, i32)> {
    let whole_months = days / DAYS_PER_MONTH_INT;
    let months = months
        .checked_add(whole_months)
        .ok_or_else(|| datetime_out_of_range("interval"))?;
    Ok((months, days - whole_months * DAYS_PER_MONTH_INT))
}

/// Trade one day between a day count and a time of opposite signs, as the `justify` functions finish.
fn align_time_with_days(days: i32, micros: i64) -> (i32, i64) {
    if days > 0 && micros < 0 {
        (days - 1, micros + USECS_PER_DAY)
    } else if days < 0 && micros > 0 {
        (days + 1, micros - USECS_PER_DAY)
    } else {
        (days, micros)
    }
}

/// `FLOAT8_FITS_IN_INT32` followed by C's truncating conversion.
fn truncate_to_i32(value: f64) -> Result<i32> {
    if value.is_nan() || !(value >= f64::from(i32::MIN) && value < -f64::from(i32::MIN)) {
        return Err(datetime_out_of_range("interval"));
    }
    Ok(value as i32)
}

/// `TSROUND`: round to the microsecond precision of a timestamp's seconds, halves to even as `rint` rounds.
fn timestamp_round(value: f64) -> f64 {
    (value * USECS_PER_SEC).round_ties_even() / USECS_PER_SEC
}

#[cfg(test)]
mod tests;
