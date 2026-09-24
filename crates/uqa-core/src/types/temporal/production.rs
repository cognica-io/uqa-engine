//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal producers use admitted text and borrowed parser tokens.

use super::{
    epoch_date, parse_interval_literal, parse_time_micros, split_offset_suffix, DateTime, Duration,
    NaiveTime, ProductionControl, ProductionString, TemporalValue, ValueRetentionError,
    MICROS_PER_DAY, MICROS_PER_SECOND,
};
use crate::memory::Produced;

impl TemporalValue {
    pub fn parse_same_kind_with_control(
        &self,
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        match self {
            Self::Date { .. } => Self::parse_date_with_control(input, control),
            Self::Time { .. } => Self::parse_time_with_control(input, control),
            Self::TimeTz { .. } => Self::parse_time_tz_with_control(input, control),
            Self::Timestamp { .. } => Self::parse_timestamp_with_control(input, control),
            Self::TimestampTz { .. } => Self::parse_timestamp_tz_with_control(input, control),
            Self::Interval { .. } => Self::parse_interval_with_control(input, control),
        }
    }

    pub fn parse_date_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        Ok(Self::try_parse_date_with_control(input, control)?.ok())
    }

    pub fn try_parse_date_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, chrono::ParseError>, ValueRetentionError> {
        check_input(input, control)?;
        let value = Self::try_parse_date(input);
        control.check()?;
        Ok(value)
    }

    pub fn parse_time_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        check_input(input, control)?;
        let value = parse_time_micros(input.trim(), control)?.map(|micros| Self::Time { micros });
        control.check()?;
        Ok(value)
    }

    pub fn parse_time_tz_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        check_input(input, control)?;
        let Some((time, offset_minutes)) = split_offset_suffix(input.trim()) else {
            return Ok(None);
        };
        let value = parse_time_micros(time.trim(), control)?.map(|micros| Self::TimeTz {
            micros,
            offset_minutes,
        });
        control.check()?;
        Ok(value)
    }

    pub fn parse_timestamp_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        check_input(input, control)?;
        let value = Self::parse_timestamp(input);
        control.check()?;
        Ok(value)
    }

    pub fn parse_timestamp_tz_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        check_input(input, control)?;
        let value = Self::parse_timestamp_tz(input);
        control.check()?;
        Ok(value)
    }

    pub fn parse_interval_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        control.check()?;
        parse_interval_literal(input, control)
    }

    pub fn to_sql_string_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        match self {
            Self::Date { days } => {
                match epoch_date().checked_add_signed(Duration::days(i64::from(*days))) {
                    Some(date) => control.format(format_args!("{date}")),
                    None => control.format(format_args!("{days}")),
                }
            }
            Self::Time { micros } => format_time(*micros, control),
            Self::TimeTz {
                micros,
                offset_minutes,
            } => {
                let mut out = ProductionString::new(*control);
                out.push_str(&format_time(*micros, control)?)?;
                let sign = if *offset_minutes < 0 { '-' } else { '+' };
                let abs = offset_minutes.unsigned_abs();
                let offset = if abs % 60 == 0 {
                    control.format(format_args!("{sign}{:02}", abs / 60))?
                } else {
                    control.format(format_args!("{sign}{:02}:{:02}", abs / 60, abs % 60))?
                };
                out.push_str(&offset)?;
                out.finish()
            }
            Self::Timestamp { micros } => format_timestamp(*micros, false, control),
            Self::TimestampTz { micros } => format_timestamp(*micros, true, control),
            Self::Interval {
                months,
                days,
                micros,
            } => format_interval(*months, *days, *micros, control),
        }
    }
}

fn check_input(input: &str, control: &ProductionControl<'_>) -> Result<(), ValueRetentionError> {
    control.check()?;
    for _ in input.as_bytes().chunks(4096) {
        control.check()?;
    }
    Ok(())
}

fn append_fraction(
    out: &mut ProductionString<'_>,
    mut fraction: u64,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    if fraction == 0 {
        return Ok(());
    }
    let mut width = 6;
    while fraction.is_multiple_of(10) {
        fraction /= 10;
        width -= 1;
    }
    out.push('.')?;
    out.push_str(&control.format(format_args!("{fraction:0width$}"))?)
}

fn format_time(
    micros: i64,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    if micros == MICROS_PER_DAY {
        return control.copy_text("24:00:00");
    }
    let normalized = micros.rem_euclid(MICROS_PER_DAY);
    let seconds = normalized / MICROS_PER_SECOND;
    let fraction = normalized % MICROS_PER_SECOND;
    let (Ok(seconds), Some(nanos)) = (
        u32::try_from(seconds),
        fraction
            .checked_mul(1_000)
            .and_then(|nanos| u32::try_from(nanos).ok()),
    ) else {
        return control.format(format_args!("{normalized}"));
    };
    let Some(time) = NaiveTime::from_num_seconds_from_midnight_opt(seconds, nanos) else {
        return control.format(format_args!("{normalized}"));
    };
    let mut out = ProductionString::new(*control);
    out.push_str(&control.format(format_args!("{}", time.format("%H:%M:%S")))?)?;
    append_fraction(&mut out, fraction as u64, control)?;
    out.finish()
}

fn format_timestamp(
    micros: i64,
    utc: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let Some(dt) = DateTime::from_timestamp_micros(micros) else {
        return control.format(format_args!("{micros}"));
    };
    let mut out = ProductionString::new(*control);
    out.push_str(&control.format(format_args!(
        "{}",
        dt.naive_utc().format("%Y-%m-%d %H:%M:%S")
    ))?)?;
    append_fraction(
        &mut out,
        micros.rem_euclid(MICROS_PER_SECOND) as u64,
        control,
    )?;
    if utc {
        out.push_str("+00")?;
    }
    out.finish()
}

fn format_interval(
    months: i32,
    days: i32,
    micros: i64,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut out = ProductionString::new(*control);
    let mut is_before = false;
    for (value, unit) in [(months / 12, "year"), (months % 12, "mon"), (days, "day")] {
        if value == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push(' ')?;
        }
        if is_before && value > 0 {
            out.push('+')?;
        }
        out.push_str(&control.format(format_args!("{value} {unit}"))?)?;
        if value != 1 {
            out.push('s')?;
        }
        is_before |= value < 0;
    }
    if micros != 0 || out.is_empty() {
        if !out.is_empty() {
            out.push(' ')?;
        }
        if micros < 0 {
            out.push('-')?;
        } else if is_before {
            out.push('+')?;
        }
        let abs = micros.unsigned_abs();
        let hours = abs / 3_600_000_000;
        let minutes = abs % 3_600_000_000 / 60_000_000;
        let seconds = abs % 60_000_000 / 1_000_000;
        out.push_str(&control.format(format_args!("{hours:02}:{minutes:02}:{seconds:02}"))?)?;
        append_fraction(&mut out, abs % 1_000_000, control)?;
    }
    out.finish()
}

#[cfg(test)]
mod tests;
