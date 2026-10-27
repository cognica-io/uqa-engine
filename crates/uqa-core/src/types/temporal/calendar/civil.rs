//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Gregorian calendar fields span the complete SQL date and timestamp carriers.

use super::{from_calendar, to_output_calendar};
use crate::types::temporal::{MICROS_PER_DAY, MICROS_PER_SECOND, POSTGRES_EPOCH_DAYS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemporalDate {
    days: i32,
}

impl TemporalDate {
    pub const fn from_unix_days(days: i32) -> Self {
        Self { days }
    }
    pub const fn unix_days(self) -> i32 {
        self.days
    }

    pub fn from_calendar(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1..=12).contains(&month) || !(1..=Self::days_in_month(year, month)).contains(&day) {
            return None;
        }
        from_calendar(i64::from(year), month, day).map(Self::from_unix_days)
    }

    pub fn year(self) -> i32 {
        to_output_calendar(self.days).0 as i32
    }
    pub fn month(self) -> u32 {
        to_output_calendar(self.days).1 as u32
    }
    pub fn month0(self) -> u32 {
        self.month() - 1
    }
    pub fn day(self) -> u32 {
        to_output_calendar(self.days).2 as u32
    }
    pub fn days_in_month(year: i32, month: u32) -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => 28 + u32::from(year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)),
            _ => 0,
        }
    }
    pub fn checked_add_days(self, days: i32) -> Option<Self> {
        self.days.checked_add(days).map(Self::from_unix_days)
    }
    pub fn weekday_sunday(self) -> u32 {
        (i64::from(self.days) + 4).rem_euclid(7) as u32
    }
    pub fn weekday_monday(self) -> u32 {
        (self.weekday_sunday() + 6) % 7
    }
    pub fn ordinal(self) -> u32 {
        let (_, month, day) = to_output_calendar(self.days);
        let preceding: u32 = (1..month as u32)
            .map(|month| Self::days_in_month(self.year(), month))
            .sum();
        preceding + day as u32
    }
    /// ISO week numbering uses the year containing the week's Thursday without adding days beyond the physical day carrier.
    pub fn iso_week(self) -> (i32, u32) {
        let year = self.year();
        let ordinal = self.ordinal() as i32;
        let weekday = self.weekday_monday() as i32;
        let january_first = (weekday - ordinal + 1).rem_euclid(7);
        let week = (ordinal + 9 - weekday) / 7;
        if week == 0 {
            let previous_year = year - 1;
            let previous_first =
                (january_first - 365 - i32::from(Self::days_in_month(previous_year, 2) == 29))
                    .rem_euclid(7);
            return (previous_year, weeks_in_year(previous_year, previous_first));
        }
        if week as u32 > weeks_in_year(year, january_first) {
            (year + 1, 1)
        } else {
            (year, week as u32)
        }
    }
    pub fn at_time(self, micros: i64) -> Option<TemporalDateTime> {
        (0..MICROS_PER_DAY)
            .contains(&micros)
            .then_some(TemporalDateTime { date: self, micros })
    }
    pub fn at_hms(self, hour: u32, minute: u32, second: u32) -> Option<TemporalDateTime> {
        self.at_hms_micro(hour, minute, second, 0)
    }
    pub fn at_hms_micro(
        self,
        hour: u32,
        minute: u32,
        second: u32,
        fraction: u32,
    ) -> Option<TemporalDateTime> {
        if hour >= 24 || minute >= 60 || second >= 60 || fraction >= 1_000_000 {
            return None;
        }
        self.at_time(
            (i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))
                * MICROS_PER_SECOND
                + i64::from(fraction),
        )
    }
}

fn weeks_in_year(year: i32, january_first: i32) -> u32 {
    52 + u32::from(
        january_first == 3 || (january_first == 2 && TemporalDate::days_in_month(year, 2) == 29),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemporalDateTime {
    date: TemporalDate,
    micros: i64,
}

impl TemporalDateTime {
    /// Decompose the complete signed timestamp payload, including physical observations outside the input domain.
    pub fn from_postgres_micros(micros: i64) -> Self {
        Self {
            date: TemporalDate::from_unix_days(
                (micros.div_euclid(MICROS_PER_DAY) + i64::from(POSTGRES_EPOCH_DAYS)) as i32,
            ),
            micros: micros.rem_euclid(MICROS_PER_DAY),
        }
    }
    pub fn postgres_micros(self) -> Option<i64> {
        let days = i128::from(self.date.days) - i128::from(POSTGRES_EPOCH_DAYS);
        i64::try_from(days * i128::from(MICROS_PER_DAY) + i128::from(self.micros)).ok()
    }
    pub const fn date(self) -> TemporalDate {
        self.date
    }
    pub const fn time_micros(self) -> i64 {
        self.micros
    }
    pub fn year(self) -> i32 {
        self.date.year()
    }
    pub fn month(self) -> u32 {
        self.date.month()
    }
    pub fn day(self) -> u32 {
        self.date.day()
    }
    pub fn hour(self) -> u32 {
        (self.micros / MICROS_PER_SECOND / 3600) as u32
    }
    pub fn minute(self) -> u32 {
        (self.micros / MICROS_PER_SECOND / 60 % 60) as u32
    }
    pub fn second(self) -> u32 {
        (self.micros / MICROS_PER_SECOND % 60) as u32
    }
    pub fn subsecond_micros(self) -> u32 {
        (self.micros % MICROS_PER_SECOND) as u32
    }
}
