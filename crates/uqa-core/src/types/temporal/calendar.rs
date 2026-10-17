//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Gregorian calendar conversion over the integer day carrier, independent of timestamp-library limits.

const UNIX_EPOCH_JULIAN: i64 = 2_440_588;
const DATE_END_JULIAN: i64 = 2_147_483_494;

pub(super) fn valid_date(days: i32) -> bool {
    (0..DATE_END_JULIAN).contains(&(i64::from(days) + UNIX_EPOCH_JULIAN))
}

/// Input has already validated its era, month, day and leap year. March starts the arithmetic year, leaving the leap day at the end of each four-year block.
pub(super) fn from_calendar(year: i64, month: u32, day: u32) -> Option<i32> {
    let year = i128::from(year) - i128::from(month <= 2);
    let era = year.div_euclid(400);
    let year_in_era = year - era * 400;
    let month = i128::from(month) + if month > 2 { -3 } else { 9 };
    let day_in_year = (153 * month + 2) / 5 + i128::from(day) - 1;
    let day_in_era = year_in_era * 365 + year_in_era / 4 - year_in_era / 100 + day_in_year;
    i32::try_from(era * 146_097 + day_in_era - 719_468).ok()
}

pub(super) fn to_calendar(days: i32) -> (i64, i64, i64) {
    let days = i64::from(days) + 719_468;
    let era = days.div_euclid(146_097);
    let day_in_era = days - era * 146_097;
    let year_in_era =
        (day_in_era - day_in_era / 1460 + day_in_era / 36_524 - day_in_era / 146_096) / 365;
    let day_in_year = day_in_era - (365 * year_in_era + year_in_era / 4 - year_in_era / 100);
    let month = (5 * day_in_year + 2) / 153;
    let day = day_in_year - (153 * month + 2) / 5 + 1;
    let month = month + if month < 10 { 3 } else { -9 };
    let year = year_in_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests;
