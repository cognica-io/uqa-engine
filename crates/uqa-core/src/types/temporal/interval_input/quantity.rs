//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact whole quantities and fractional carries shared by interval grammars.

use super::{IntervalInput, TemporalInputError, TemporalValue, MICROS_PER_DAY, MICROS_PER_SECOND};

#[derive(Clone, Copy)]
pub(super) struct Quantity {
    pub whole: i64,
    pub fraction: f64,
}

impl Quantity {
    pub fn integer(whole: i64) -> Self {
        Self {
            whole,
            fraction: 0.0,
        }
    }

    pub fn decimal(text: &str) -> IntervalInput<Self> {
        if text.starts_with(['+', '-']) && !text.bytes().any(|c| c.is_ascii_digit()) {
            return Err(TemporalInputError::InvalidSyntax);
        }
        let (integer, decimal) = text
            .split_once('.')
            .map_or((text, None), |(i, f)| (i, Some(f)));
        let whole = if matches!(integer, "" | "+" | "-") && decimal.is_some() {
            0
        } else {
            integer.parse::<i64>().map_err(|error| match error.kind() {
                std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
                    TemporalInputError::IntervalFieldOverflow
                }
                _ => TemporalInputError::InvalidSyntax,
            })?
        };
        let mut fraction = match decimal {
            None | Some("") => 0.0,
            Some(digits) if digits.bytes().all(|c| c.is_ascii_digit()) => {
                let point = text.find('.').expect("fraction separator");
                text[point..]
                    .parse::<f64>()
                    .map_err(|_| TemporalInputError::InvalidSyntax)?
            }
            Some(_) => return Err(TemporalInputError::InvalidSyntax),
        };
        if text.starts_with('-') {
            fraction = -fraction;
        }
        Ok(Self { whole, fraction })
    }

    pub fn force_negative(self, force: bool) -> Self {
        if !force {
            return self;
        }
        Self {
            whole: if self.whole > 0 {
                -self.whole
            } else {
                self.whole
            },
            fraction: -self.fraction.abs(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Unit {
    Micros,
    Millis,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
    Decade,
    Century,
    Millennium,
}

impl Unit {
    pub const TIME_MASK: u16 = (1 << Self::Micros as u16)
        | (1 << Self::Millis as u16)
        | (1 << Self::Second as u16)
        | (1 << Self::Minute as u16)
        | (1 << Self::Hour as u16);

    pub fn parse(word: &str) -> IntervalInput<Self> {
        Ok(match word {
            "microsecond" | "microseconds" | "us" => Self::Micros,
            "millisecond" | "milliseconds" | "ms" => Self::Millis,
            "second" | "seconds" | "sec" | "secs" | "s" => Self::Second,
            "minute" | "minutes" | "min" | "mins" | "m" => Self::Minute,
            "hour" | "hours" | "hr" | "hrs" | "h" => Self::Hour,
            "day" | "days" | "d" => Self::Day,
            "week" | "weeks" | "w" => Self::Week,
            "month" | "months" | "mon" | "mons" => Self::Month,
            "year" | "years" | "yr" | "yrs" | "y" => Self::Year,
            "decade" | "decades" => Self::Decade,
            "century" | "centuries" => Self::Century,
            "millennium" | "millenniums" | "millennia" => Self::Millennium,
            _ => return Err(TemporalInputError::InvalidSyntax),
        })
    }

    pub fn mask(self, quantity: Quantity) -> u16 {
        if matches!(self, Self::Second) && quantity.fraction != 0.0 {
            7
        } else {
            1 << self as u16
        }
    }
}

#[derive(Default)]
pub(super) struct IntervalFields {
    years: i32,
    months: i32,
    days: i32,
    micros: i64,
}

impl IntervalFields {
    pub fn add(&mut self, unit: Unit, number: Quantity) -> IntervalInput<()> {
        let Quantity { whole, fraction } = number;
        match unit {
            Unit::Micros => self.time(whole, fraction, 1),
            Unit::Millis => self.time(whole, fraction, 1_000),
            Unit::Second => self.time(whole, fraction, MICROS_PER_SECOND),
            Unit::Minute => self.time(whole, fraction, 60 * MICROS_PER_SECOND),
            Unit::Hour => self.time(whole, fraction, 3_600 * MICROS_PER_SECOND),
            Unit::Day | Unit::Week => {
                let scale = if matches!(unit, Unit::Week) { 7 } else { 1 };
                add_i32(&mut self.days, whole, scale)?;
                self.fractional_days(fraction, scale)
            }
            Unit::Month => {
                add_i32(&mut self.months, whole, 1)?;
                self.fractional_days(fraction, 30)
            }
            Unit::Year | Unit::Decade | Unit::Century | Unit::Millennium => {
                let scale = match unit {
                    Unit::Decade => 10,
                    Unit::Century => 100,
                    Unit::Millennium => 1_000,
                    _ => 1,
                };
                add_i32(&mut self.years, whole, scale)?;
                add_i32(
                    &mut self.months,
                    (fraction * f64::from(scale) * 12.0).round_ties_even() as i64,
                    1,
                )
            }
        }
    }

    fn fractional_days(&mut self, fraction: f64, scale: i32) -> IntervalInput<()> {
        let days = fraction * f64::from(scale);
        let whole = days.trunc() as i64;
        add_i32(&mut self.days, whole, 1)?;
        self.fractional_micros(days - whole as f64, MICROS_PER_DAY)
    }

    pub fn time(&mut self, whole: i64, fraction: f64, scale: i64) -> IntervalInput<()> {
        self.micros = whole
            .checked_mul(scale)
            .and_then(|v| self.micros.checked_add(v))
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        self.fractional_micros(fraction, scale)
    }

    fn fractional_micros(&mut self, fraction: f64, scale: i64) -> IntervalInput<()> {
        let scaled = fraction * scale as f64;
        let mut micros = scaled.trunc() as i64;
        let rest = scaled - micros as f64;
        // DecodeInterval rounds a fractional microsecond only beyond a half.
        if rest > 0.5 {
            micros += 1;
        } else if rest < -0.5 {
            micros -= 1;
        }
        self.micros = self
            .micros
            .checked_add(micros)
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        Ok(())
    }

    pub fn negate(&mut self) -> IntervalInput<()> {
        self.years = self
            .years
            .checked_neg()
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        self.months = self
            .months
            .checked_neg()
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        self.days = self
            .days
            .checked_neg()
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        self.micros = self
            .micros
            .checked_neg()
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        Ok(())
    }

    pub fn finish(self) -> IntervalInput<TemporalValue> {
        let months = i32::try_from(i64::from(self.years) * 12 + i64::from(self.months))
            .map_err(|_| TemporalInputError::IntervalOutOfRange)?;
        Ok(TemporalValue::Interval {
            months,
            days: self.days,
            micros: self.micros,
        })
    }
}

fn add_i32(total: &mut i32, whole: i64, scale: i32) -> IntervalInput<()> {
    *total = i32::try_from(whole)
        .ok()
        .and_then(|v| v.checked_mul(scale))
        .and_then(|v| total.checked_add(v))
        .ok_or(TemporalInputError::IntervalFieldOverflow)?;
    Ok(())
}
