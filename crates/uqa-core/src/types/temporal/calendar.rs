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

/// DATE output observes the retained physical Julian word, including values produced by descriptor reinterpretation rather than accepted by date input. The unsigned cycles match `PostgreSQL`'s `j2date`; admitted dates agree with the mathematical Gregorian inverse.
pub(super) fn to_output_calendar(days: i32) -> (i64, i64, i64) {
    let mut julian = (i64::from(days) + UNIX_EPOCH_JULIAN) as u32;
    julian = julian.wrapping_add(32_044);
    let centuries = julian / 146_097;
    let remainder = (julian % 146_097) * 4 + 3;
    julian = julian.wrapping_add(60 + centuries * 3 + remainder / 146_097);
    let four_years = julian / 1461;
    let day_in_cycle = julian % 1461;
    let year_in_cycle = day_in_cycle * 4 / 1461;
    let month_day = if year_in_cycle == 0 {
        (day_in_cycle + 306) % 366
    } else {
        (day_in_cycle + 305) % 365
    } + 123;
    let month_index = month_day * 2141 / 65_536;
    (
        i64::from(year_in_cycle + four_years * 4) - 4800,
        i64::from((month_index + 10) % 12 + 1),
        i64::from(month_day - 7834 * month_index / 256),
    )
}

#[cfg(test)]
mod tests;
