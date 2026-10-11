//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Interval text output. Style changes presentation, never the stored three-field carrier.

use super::{production::append_fraction, TemporalValue, MICROS_PER_SECOND};
use crate::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

/// The four interval input/output conventions of `PostgreSQL`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TemporalIntervalStyle {
    #[default]
    Postgres,
    PostgresVerbose,
    SqlStandard,
    Iso8601,
}

impl TemporalValue {
    /// Format an interval with an explicit style; other temporal kinds retain their ordinary output. Canonical storage callers continue to use `to_sql_string_with_control`.
    pub fn to_sql_string_in_interval_style_with_control(
        &self,
        style: TemporalIntervalStyle,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        control.check()?;
        if let Some(infinity) = self.infinity() {
            return control.copy_text(infinity.as_str());
        }
        match self {
            Self::Interval {
                months,
                days,
                micros,
            } => format_interval(*months, *days, *micros, style, control),
            _ => self.to_sql_string_with_control(control),
        }
    }
}

pub(super) fn format_interval(
    months: i32,
    days: i32,
    micros: i64,
    style: TemporalIntervalStyle,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut out = ProductionString::new(*control);
    let date = [
        i64::from(months / 12),
        i64::from(months % 12),
        i64::from(days),
    ];
    let time = [
        micros / (3_600 * MICROS_PER_SECOND),
        micros % (3_600 * MICROS_PER_SECOND) / (60 * MICROS_PER_SECOND),
        micros % (60 * MICROS_PER_SECOND) / MICROS_PER_SECOND,
    ];
    let fraction = (micros % MICROS_PER_SECOND).unsigned_abs();
    match style {
        TemporalIntervalStyle::Postgres => {
            let mut before = false;
            for (value, unit) in date.into_iter().zip(["year", "mon", "day"]) {
                if value == 0 {
                    continue;
                }
                if !out.is_empty() {
                    out.push(' ')?;
                }
                if before && value > 0 {
                    out.push('+')?;
                }
                integer_unit(&mut out, value, unit, control)?;
                // Each nonzero field determines the sign prefix of only the next field.
                before = value < 0;
            }
            if micros != 0 || out.is_empty() {
                if !out.is_empty() {
                    out.push(' ')?;
                }
                if micros < 0 {
                    out.push('-')?;
                } else if before {
                    out.push('+')?;
                }
                clock(&mut out, time, fraction, true, control)?;
            }
        }
        TemporalIntervalStyle::SqlStandard => {
            sql_standard(&mut out, date, time, micros, control)?;
        }
        TemporalIntervalStyle::Iso8601 => {
            if months == 0 && days == 0 && micros == 0 {
                out.push_str("PT0S")?;
            } else {
                out.push('P')?;
                for (value, unit) in date.into_iter().zip(['Y', 'M', 'D']) {
                    iso_unit(&mut out, value, unit, control)?;
                }
                if micros != 0 {
                    out.push('T')?;
                    iso_unit(&mut out, time[0], 'H', control)?;
                    iso_unit(&mut out, time[1], 'M', control)?;
                    if time[2] != 0 || fraction != 0 {
                        if micros < 0 {
                            out.push('-')?;
                        }
                        out.push_str(&control.format(format_args!("{}", time[2].unsigned_abs()))?)?;
                        append_fraction(&mut out, fraction, control)?;
                        out.push('S')?;
                    }
                }
            }
        }
        TemporalIntervalStyle::PostgresVerbose => {
            postgres_verbose(&mut out, date, time, micros, control)?;
        }
    }
    out.finish()
}

fn sql_standard(
    out: &mut ProductionString<'_>,
    date: [i64; 3],
    time: [i64; 3],
    micros: i64,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    let months = date[0] * 12 + date[1];
    let days = date[2];
    let fraction = (micros % MICROS_PER_SECOND).unsigned_abs();
    let negative = months < 0 || days < 0 || micros < 0;
    let positive = months > 0 || days > 0 || micros > 0;
    let standard = !(negative && positive || months != 0 && (days != 0 || micros != 0));
    if !negative && !positive {
        out.push('0')?;
    } else if !standard {
        out.push_str(&control.format(format_args!(
            "{}{}-{} {}{} {}",
            sign(months),
            date[0].unsigned_abs(),
            date[1].unsigned_abs(),
            sign(days),
            days.unsigned_abs(),
            sign(micros),
        ))?)?;
        clock(out, time, fraction, false, control)?;
    } else {
        if negative {
            out.push('-')?;
        }
        if months != 0 {
            out.push_str(&control.format(format_args!(
                "{}-{}",
                date[0].unsigned_abs(),
                date[1].unsigned_abs(),
            ))?)?;
        } else {
            if days != 0 {
                out.push_str(&control.format(format_args!("{} ", days.unsigned_abs()))?)?;
            }
            clock(out, time, fraction, false, control)?;
        }
    }
    Ok(())
}

fn postgres_verbose(
    out: &mut ProductionString<'_>,
    date: [i64; 3],
    time: [i64; 3],
    micros: i64,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    let fraction = (micros % MICROS_PER_SECOND).unsigned_abs();
    out.push('@')?;
    let before = date
        .into_iter()
        .chain([micros])
        .find(|value| *value != 0)
        .is_some_and(|value| value < 0);
    let direction = if before { -1 } else { 1 };
    for (value, unit) in [date[0], date[1], date[2], time[0], time[1]]
        .into_iter()
        .zip(["year", "mon", "day", "hour", "min"])
    {
        if value != 0 {
            out.push(' ')?;
            integer_unit(out, value * direction, unit, control)?;
        }
    }
    if time[2] != 0 || fraction != 0 {
        out.push(' ')?;
        if (micros < 0) != before {
            out.push('-')?;
        }
        out.push_str(&control.format(format_args!("{}", time[2].unsigned_abs()))?)?;
        append_fraction(out, fraction, control)?;
        out.push_str(if time[2].unsigned_abs() == 1 && fraction == 0 {
            " sec"
        } else {
            " secs"
        })?;
    }
    if date == [0; 3] && micros == 0 {
        out.push_str(" 0")?;
    }
    if before {
        out.push_str(" ago")?;
    }
    Ok(())
}

fn sign(value: i64) -> char {
    if value < 0 {
        '-'
    } else {
        '+'
    }
}

fn integer_unit(
    out: &mut ProductionString<'_>,
    value: i64,
    unit: &str,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    out.push_str(&control.format(format_args!("{value} {unit}"))?)?;
    if value != 1 {
        out.push('s')?;
    }
    Ok(())
}

fn iso_unit(
    out: &mut ProductionString<'_>,
    value: i64,
    unit: char,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    if value != 0 {
        out.push_str(&control.format(format_args!("{value}{unit}"))?)?;
    }
    Ok(())
}

fn clock(
    out: &mut ProductionString<'_>,
    time: [i64; 3],
    fraction: u64,
    padded_hour: bool,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    let [hour, minute, second] = time.map(i64::unsigned_abs);
    let width = if padded_hour { 2 } else { 1 };
    out.push_str(&control.format(format_args!("{hour:0width$}:{minute:02}:{second:02}"))?)?;
    append_fraction(out, fraction, control)
}

#[cfg(test)]
mod tests;
