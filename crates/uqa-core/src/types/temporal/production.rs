//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal producers use admitted text and borrowed parser tokens.

use super::{
    wall_clock_micros, ProductionControl, ProductionString, TemporalDateOrder, TemporalInputError,
    TemporalValue, ValueRetentionError, MICROS_PER_DAY, MICROS_PER_SECOND,
};
use crate::memory::Produced;
use chrono::NaiveTime;

impl TemporalValue {
    pub fn parse_same_kind_with_control(
        &self,
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        self.parse_same_kind_at_with_control(input, wall_clock_micros(), control)
    }

    /// Parse this temporal family using the caller's Unix-microsecond clock for special input values.
    pub fn parse_same_kind_at_with_control(
        &self,
        input: &str,
        now_micros: i64,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        self.parse_same_kind_in_order_with_control(
            input,
            now_micros,
            TemporalDateOrder::default(),
            control,
        )
    }

    /// Parse this family using the caller's date order and transaction clock.
    pub fn parse_same_kind_in_order_with_control(
        &self,
        input: &str,
        now_micros: i64,
        order: TemporalDateOrder,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        Ok(match self {
            Self::Date { .. } => {
                Self::date_input_in_order_with_control(input, now_micros, order, control)?
            }
            Self::Time { .. } => {
                Self::time_input_in_order_with_control(input, now_micros, order, control)?
            }
            Self::TimeTz { .. } => {
                Self::time_tz_input_in_order_with_control(input, now_micros, order, control)?
            }
            Self::Timestamp { .. } => {
                Self::timestamp_input_in_order_with_control(input, now_micros, order, control)?
            }
            Self::TimestampTz { .. } => {
                Self::timestamp_tz_input_in_order_with_control(input, now_micros, order, control)?
            }
            Self::Interval { .. } => Self::interval_input_with_control(input, control)?,
        }
        .ok())
    }

    pub fn parse_date_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        Ok(Self::date_input_with_control(input, wall_clock_micros(), control)?.ok())
    }

    pub fn parse_time_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        Ok(Self::time_input_with_control(input, wall_clock_micros(), control)?.ok())
    }

    pub fn parse_time_tz_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        Ok(Self::time_tz_input_with_control(input, wall_clock_micros(), control)?.ok())
    }

    pub fn parse_timestamp_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        Ok(Self::timestamp_input_with_control(input, wall_clock_micros(), control)?.ok())
    }

    pub fn parse_timestamp_tz_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        Ok(Self::timestamp_tz_input_with_control(input, wall_clock_micros(), control)?.ok())
    }

    pub fn parse_interval_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, ValueRetentionError> {
        Ok(Self::interval_input_with_control(input, control)?.ok())
    }

    /// `date_in` under production limits, with `now_micros` as the transaction start the special values name; the reading itself allocates nothing, so only cancellation is checked.
    pub fn date_input_with_control(
        text: &str,
        now_micros: i64,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        Self::date_input_in_order_with_control(
            text,
            now_micros,
            TemporalDateOrder::default(),
            control,
        )
    }

    /// Read this temporal family with explicit date order under production limits.
    pub fn date_input_in_order_with_control(
        text: &str,
        now_micros: i64,
        order: TemporalDateOrder,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        check_input(text, control)?;
        let value = Self::date_input_in_order(text, now_micros, order);
        control.check()?;
        Ok(value)
    }

    /// `time_in` under production limits.
    pub fn time_input_with_control(
        text: &str,
        now_micros: i64,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        Self::time_input_in_order_with_control(
            text,
            now_micros,
            TemporalDateOrder::default(),
            control,
        )
    }

    /// Read this temporal family with explicit date order under production limits.
    pub fn time_input_in_order_with_control(
        text: &str,
        now_micros: i64,
        order: TemporalDateOrder,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        check_input(text, control)?;
        let value = Self::time_input_in_order(text, now_micros, order);
        control.check()?;
        Ok(value)
    }

    /// `timetz_in` under production limits.
    pub fn time_tz_input_with_control(
        text: &str,
        now_micros: i64,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        Self::time_tz_input_in_order_with_control(
            text,
            now_micros,
            TemporalDateOrder::default(),
            control,
        )
    }

    /// Read this temporal family with explicit date order under production limits.
    pub fn time_tz_input_in_order_with_control(
        text: &str,
        now_micros: i64,
        order: TemporalDateOrder,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        check_input(text, control)?;
        let value = Self::time_tz_input_in_order(text, now_micros, order);
        control.check()?;
        Ok(value)
    }

    /// `timestamp_in` under production limits.
    pub fn timestamp_input_with_control(
        text: &str,
        now_micros: i64,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        Self::timestamp_input_in_order_with_control(
            text,
            now_micros,
            TemporalDateOrder::default(),
            control,
        )
    }

    /// Read this temporal family with explicit date order under production limits.
    pub fn timestamp_input_in_order_with_control(
        text: &str,
        now_micros: i64,
        order: TemporalDateOrder,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        check_input(text, control)?;
        let value = Self::timestamp_input_in_order(text, now_micros, order);
        control.check()?;
        Ok(value)
    }

    /// `timestamptz_in` under production limits.
    pub fn timestamp_tz_input_with_control(
        text: &str,
        now_micros: i64,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        Self::timestamp_tz_input_in_order_with_control(
            text,
            now_micros,
            TemporalDateOrder::default(),
            control,
        )
    }

    /// Read this temporal family with explicit date order under production limits.
    pub fn timestamp_tz_input_in_order_with_control(
        text: &str,
        now_micros: i64,
        order: TemporalDateOrder,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        check_input(text, control)?;
        let value = Self::timestamp_tz_input_in_order(text, now_micros, order);
        control.check()?;
        Ok(value)
    }

    /// `interval_in` under production limits: the lower-cased copy of the text is admitted against the allowance.
    pub fn interval_input_with_control(
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        control.check()?;
        Self::interval_input_in_style_with_control(
            text,
            super::TemporalIntervalStyle::Postgres,
            control,
        )
    }

    /// Read an interval with the selected SQL sign convention; ISO 8601 input is recognized in every style.
    pub fn interval_input_in_style_with_control(
        text: &str,
        style: super::TemporalIntervalStyle,
        control: &ProductionControl<'_>,
    ) -> Result<Result<Self, TemporalInputError>, ValueRetentionError> {
        control.check()?;
        super::interval_input::parse_interval_literal(text, style, control)
    }

    pub fn to_sql_string_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        if let Some(infinity) = self.infinity() {
            return control.copy_text(infinity.as_str());
        }
        match self {
            Self::Date { days } => super::date_style::format_date(
                *days,
                super::TemporalDateStyle::Iso,
                super::TemporalDateOrder::YearMonthDay,
                control,
            ),
            Self::Time { micros } => format_time(*micros, control),
            Self::TimeTz {
                micros,
                offset_seconds,
            } => {
                let mut out = ProductionString::new(*control);
                out.push_str(&format_time(*micros, control)?)?;
                let sign = if *offset_seconds < 0 { '-' } else { '+' };
                let abs = offset_seconds.unsigned_abs();
                let offset = if abs % 3600 == 0 {
                    control.format(format_args!("{sign}{:02}", abs / 3600))?
                } else if abs % 60 == 0 {
                    control.format(format_args!("{sign}{:02}:{:02}", abs / 3600, abs / 60 % 60))?
                } else {
                    control.format(format_args!(
                        "{sign}{:02}:{:02}:{:02}",
                        abs / 3600,
                        abs / 60 % 60,
                        abs % 60
                    ))?
                };
                out.push_str(&offset)?;
                out.finish()
            }
            Self::Timestamp {
                postgres_micros: micros,
            } => format_timestamp(*micros, false, control),
            Self::TimestampTz {
                postgres_micros: micros,
            } => format_timestamp(*micros, true, control),
            Self::Interval {
                months,
                days,
                micros,
            } => super::interval_style::format_interval(
                *months,
                *days,
                *micros,
                super::TemporalIntervalStyle::Postgres,
                control,
            ),
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

pub(super) fn append_fraction(
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
    // Every day in the signed microsecond carrier fits in the integer day carrier.
    let days = micros.div_euclid(MICROS_PER_DAY) as i32 + super::POSTGRES_EPOCH_DAYS;
    let (year, month, day) = super::calendar::to_output_calendar(days);
    let seconds = micros.rem_euclid(MICROS_PER_DAY) / MICROS_PER_SECOND;
    let mut out = ProductionString::new(*control);
    push_date(&mut out, year, month, day, control)?;
    out.push(' ')?;
    out.push_str(&control.format(format_args!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    ))?)?;
    append_fraction(
        &mut out,
        micros.rem_euclid(MICROS_PER_SECOND) as u64,
        control,
    )?;
    if utc {
        out.push_str("+00")?;
    }
    push_era(&mut out, year)?;
    out.finish()
}

/// `EncodeDateOnly`'s calendar fields: a year of at least four digits without a sign, counting years before the common era from 1 BC as `-(year - 1)` does, followed by the month and day.
fn push_date(
    out: &mut ProductionString<'_>,
    year: i64,
    month: i64,
    day: i64,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    let displayed = if year > 0 { year } else { 1 - year };
    out.push_str(&control.format(format_args!("{displayed:04}-{month:02}-{day:02}"))?)
}

/// The ` BC` suffix `EncodeDateOnly` and `EncodeDateTime` append to a year at or before zero.
fn push_era(out: &mut ProductionString<'_>, year: i64) -> Result<(), ValueRetentionError> {
    if year <= 0 {
        out.push_str(" BC")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
