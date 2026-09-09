//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

/// The stored-field restriction in a `PostgreSQL` interval declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u16)]
pub enum IntervalFields {
    All = 32767,
    Year = 4,
    Month = 2,
    Day = 8,
    Hour = 1024,
    Minute = 2048,
    Second = 4096,
    YearToMonth = 6,
    DayToHour = 1032,
    DayToMinute = 3080,
    DayToSecond = 7176,
    HourToMinute = 3072,
    HourToSecond = 7168,
    MinuteToSecond = 6144,
}

impl IntervalFields {
    #[must_use]
    pub const fn modifier_mask(self) -> u16 {
        self as u16
    }

    #[must_use]
    pub const fn from_modifier_mask(mask: i64) -> Option<Self> {
        Some(match mask {
            32767 => Self::All,
            4 => Self::Year,
            2 => Self::Month,
            8 => Self::Day,
            1024 => Self::Hour,
            2048 => Self::Minute,
            4096 => Self::Second,
            6 => Self::YearToMonth,
            1032 => Self::DayToHour,
            3080 => Self::DayToMinute,
            7176 => Self::DayToSecond,
            3072 => Self::HourToMinute,
            7168 => Self::HourToSecond,
            6144 => Self::MinuteToSecond,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn sql_suffix(self) -> &'static str {
        match self {
            Self::All => "",
            Self::Year => " year",
            Self::Month => " month",
            Self::Day => " day",
            Self::Hour => " hour",
            Self::Minute => " minute",
            Self::Second => " second",
            Self::YearToMonth => " year to month",
            Self::DayToHour => " day to hour",
            Self::DayToMinute => " day to minute",
            Self::DayToSecond => " day to second",
            Self::HourToMinute => " hour to minute",
            Self::HourToSecond => " hour to second",
            Self::MinuteToSecond => " minute to second",
        }
    }

    #[must_use]
    pub fn from_sql_suffix(suffix: &str) -> Option<Self> {
        Some(match suffix.trim() {
            "" => Self::All,
            "year" => Self::Year,
            "month" => Self::Month,
            "day" => Self::Day,
            "hour" => Self::Hour,
            "minute" => Self::Minute,
            "second" => Self::Second,
            "year to month" => Self::YearToMonth,
            "day to hour" => Self::DayToHour,
            "day to minute" => Self::DayToMinute,
            "day to second" => Self::DayToSecond,
            "hour to minute" => Self::HourToMinute,
            "hour to second" => Self::HourToSecond,
            "minute to second" => Self::MinuteToSecond,
            _ => return None,
        })
    }
}
