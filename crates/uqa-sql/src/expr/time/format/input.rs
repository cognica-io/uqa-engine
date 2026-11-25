//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read `PostgreSQL` date templates against the same calendar used by date input and output.

use super::{Item, Template, DAYS, MONTHS};
use crate::error::{Result, SQLError};
use uqa_core::{memory::ProductionControl, TemporalDate, TemporalValue};

#[derive(Default)]
struct Fields {
    year: i32,
    month: i32,
    day: i32,
    ordinal: i32,
    weekday: i32,
    era: i32,
    hour: i32,
    minute: i32,
    second: i32,
    millisecond: i32,
    microsecond: i32,
    meridiem: i32,
    twelve_hour: bool,
}

fn diagnostic(
    state: &str,
    message: String,
    detail: Option<String>,
    hint: Option<&str>,
) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: state.into(),
        message,
        detail,
        hint: hint.map(str::to_owned),
    }
}

fn input_error(
    state: &str,
    message: std::fmt::Arguments<'_>,
    detail: Option<&str>,
    control: &ProductionControl<'_>,
) -> SQLError {
    control
        .format(message)
        .and_then(|message| SQLError::produced_diagnostic(state, message, detail, control))
        .unwrap_or_else(SQLError::from)
}

fn set(field: &mut i32, value: i32, spelling: &str) -> Result<()> {
    if *field != 0 && *field != value {
        return Err(diagnostic(
            "22007",
            format!("conflicting values for \"{spelling}\" field in formatting string"),
            Some("This value contradicts a previous setting for the same field type.".into()),
            None,
        ));
    }
    *field = value;
    Ok(())
}

fn skip_spaces(input: &mut &str, control: &ProductionControl<'_>) -> Result<usize> {
    let mut length = 0_usize;
    for _ in input.bytes().take_while(u8::is_ascii_whitespace) {
        if length.is_multiple_of(1024) {
            control.check()?;
        }
        length += 1;
    }
    *input = &input[length..];
    Ok(length)
}

fn number(
    input: &mut &str,
    width: usize,
    fixed: bool,
    spelling: &str,
    control: &ProductionControl<'_>,
) -> Result<(i32, usize)> {
    let leading = skip_spaces(input, control)?;
    let text = *input;
    let mut end = width.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = &text[..end];
    const WIDTH_HINT: &str =
        "If your source string is not fixed-width, try using the \"FM\" modifier.";
    if fixed && text.len() < width {
        return Err(diagnostic(
            "22007",
            format!("source string too short for \"{spelling}\" formatting field"),
            Some(format!(
                "Field requires {width} characters, but only {} remain.",
                text.len()
            )),
            Some(WIDTH_HINT),
        ));
    }
    let source = if fixed { prefix } else { text };
    let digits = source.strip_prefix(['-', '+']).unwrap_or(source);
    let mut length = 0;
    let sign = if source.starts_with('-') { -1 } else { 1 };
    let mut value = Some(0_i32);
    for byte in digits.bytes() {
        if !byte.is_ascii_digit() {
            break;
        }
        if length % 1024 == 0 {
            control.check()?;
        }
        length += 1;
        let digit = sign * i32::from(byte - b'0');
        value = value.and_then(|v| v.checked_mul(10)?.checked_add(digit));
    }
    let used = source.len() - digits.len() + length;
    if length == 0 || (fixed && used != width) {
        let (detail, hint) = if length == 0 {
            ("Value must be an integer.".into(), None)
        } else {
            (
                format!("Field requires {width} characters, but only {used} could be parsed."),
                Some(WIDTH_HINT),
            )
        };
        return Err(diagnostic(
            "22007",
            format!("invalid value \"{prefix}\" for \"{spelling}\""),
            Some(detail),
            hint,
        ));
    }
    let value = value.ok_or_else(|| {
        diagnostic(
            "22008",
            format!("value for \"{spelling}\" in source string is out of range"),
            Some(format!(
                "Value must be in the range {} to {}.",
                i32::MIN,
                i32::MAX
            )),
            None,
        )
    })?;
    *input = &text[used..];
    Ok((value, leading + used))
}

fn word(
    input: &mut &str,
    words: &[&str],
    abbreviated: bool,
    spelling: &str,
    control: &ProductionControl<'_>,
) -> Result<i32> {
    for (index, name) in words.iter().enumerate() {
        let name = if abbreviated { &name[..3] } else { name };
        if input
            .get(..name.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(name))
        {
            *input = &input[name.len()..];
            return Ok(index as i32);
        }
    }
    let mut end = 0;
    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            break;
        }
        if end % 1024 == 0 {
            control.check()?;
        }
        end += 1;
    }
    let invalid = &input[..end];
    Err(input_error(
        "22007",
        format_args!("invalid value \"{invalid}\" for \"{spelling}\""),
        Some("The given value did not match any of the allowed values for this field."),
        control,
    ))
}

impl Fields {
    fn read(
        &mut self,
        name: &str,
        spelling: &str,
        input: &mut &str,
        fixed: bool,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        match name {
            "MONTH" | "MON" => {
                return set(
                    &mut self.month,
                    word(input, &MONTHS, name == "MON", spelling, control)? + 1,
                    spelling,
                )
            }
            "DAY" | "DY" => {
                return set(
                    &mut self.weekday,
                    word(input, &DAYS, name == "DY", spelling, control)? + 1,
                    spelling,
                )
            }
            "AD" | "BC" => {
                return set(
                    &mut self.era,
                    word(input, &["AD", "BC"], false, spelling, control)?,
                    spelling,
                )
            }
            "AM" | "PM" => {
                self.twelve_hour = true;
                return set(
                    &mut self.meridiem,
                    word(input, &["AM", "PM"], false, spelling, control)?,
                    spelling,
                );
            }
            _ => {}
        }
        let width = match name {
            "YYYY" => 4,
            "DDD" | "MS" => 3,
            "US" => 6,
            _ => 2,
        };
        let (mut value, used) = number(input, width, fixed, spelling, control)?;
        let field = match name {
            "YYYY" | "YY" => {
                if name == "YY" && used < 4 {
                    value += match value {
                        ..70 => 2000,
                        70..100 => 1900,
                        100..520 => 2000,
                        520..1000 => 1000,
                        _ => 0,
                    };
                }
                &mut self.year
            }
            "MM" => &mut self.month,
            "DD" => &mut self.day,
            "DDD" => &mut self.ordinal,
            "HH12" | "HH" | "HH24" => {
                if name != "HH24" {
                    self.twelve_hour = true;
                }
                &mut self.hour
            }
            "MI" => &mut self.minute,
            "SS" => &mut self.second,
            "US" | "MS" => {
                value *= 10_i32.pow(width.saturating_sub(used) as u32);
                if name == "US" {
                    &mut self.microsecond
                } else {
                    &mut self.millisecond
                }
            }
            _ => unreachable!("recognized numeric template field"),
        };
        set(field, value, spelling)
    }

    fn date(self, text: &str, control: &ProductionControl<'_>) -> Result<TemporalValue> {
        let field_overflow = || {
            input_error(
                "22008",
                format_args!("date/time field value out of range: \"{text}\""),
                None,
                control,
            )
        };
        if self.twelve_hour && !(1..=12).contains(&self.hour) {
            return Err(diagnostic(
                "22007",
                format!("hour \"{}\" is invalid for the 12-hour clock", self.hour),
                None,
                Some("Use the 24-hour clock, or give an hour between 1 and 12."),
            ));
        }
        let fraction = i64::from(self.millisecond) * 1000 + i64::from(self.microsecond);
        if !(0..24).contains(&self.hour)
            || !(0..60).contains(&self.minute)
            || !(0..60).contains(&self.second)
            || !(0..1_000_000).contains(&fraction)
        {
            return Err(field_overflow());
        }
        let year = if self.era == 1 {
            self.year.checked_neg().ok_or_else(field_overflow)?
        } else {
            self.year
        };
        let year = year + i32::from(year < 0);
        let mut month = if self.month == 0 { 1 } else { self.month };
        let mut day = if self.day == 0 { 1 } else { self.day };
        if self.ordinal != 0 && (month <= 1 || day <= 1) {
            if year == 0 && self.era == 0 {
                return Err(diagnostic(
                    "22007",
                    "cannot calculate day of year without year information".into(),
                    None,
                    None,
                ));
            }
            let jan1 = TemporalDate::from_calendar(year, 1, 1).ok_or_else(field_overflow)?;
            let ordinal = self
                .ordinal
                .checked_sub(1)
                .and_then(|d| jan1.checked_add_days(d))
                .filter(|d| d.year() == year)
                .ok_or_else(field_overflow)?;
            if month <= 1 {
                month = ordinal.month() as i32;
            }
            if day <= 1 {
                day = ordinal.day() as i32;
            }
        }
        if !(1..=12).contains(&month)
            || !(1..=TemporalDate::days_in_month(year, month as u32) as i32).contains(&day)
        {
            return Err(field_overflow());
        }
        let date = TemporalDate::from_calendar(year, month as u32, day as u32)
            .filter(|d| TemporalValue::date_days_in_range(d.unix_days()))
            .ok_or_else(|| {
                input_error(
                    "22008",
                    format_args!("date out of range: \"{text}\""),
                    None,
                    control,
                )
            })?;
        Ok(TemporalValue::Date {
            days: date.unix_days(),
        })
    }
}

pub(in crate::expr) fn parse_date(
    text: &str,
    template: &str,
    control: &ProductionControl<'_>,
) -> Result<TemporalValue> {
    let mut tokens = Template::new(template);
    let mut next_token = tokens.next(control)?;
    let mut input = text;
    let mut fields = Fields::default();
    let mut exact = false;
    let mut first = true;
    let mut skipped: i64 = 0;
    control.check()?;
    while let Some(token) = next_token {
        if input.is_empty() {
            break;
        }
        next_token = tokens.next(control)?;
        if !exact
            && (first || matches!(token, Item::Field { .. }))
            && !matches!(token, Item::Field { name: "FX", .. })
        {
            skipped += skip_spaces(&mut input, control)? as i64;
        }
        first = false;
        match token {
            Item::Field { name: "FX", .. } => exact = true,
            Item::Field {
                name,
                spelling,
                fill,
            } => {
                let next_digit = match next_token {
                    Some(Item::Field { name, .. }) => !matches!(
                        name,
                        "DAY" | "DY" | "MON" | "MONTH" | "AM" | "PM" | "AD" | "BC" | "FX"
                    ),
                    Some(Item::Literal { ch, .. }) => ch.is_ascii_digit(),
                    None => false,
                };
                fields.read(name, spelling, &mut input, !fill && next_digit, control)?;
            }
            Item::Literal { ch, quoted } => {
                if let Some(next) = input.chars().next() {
                    if !exact && !quoted && (ch.is_ascii_whitespace() || ch.is_ascii_punctuation())
                    {
                        if !next.is_ascii_whitespace() && !next.is_ascii_punctuation() {
                            skipped -= 1;
                            continue;
                        }
                    } else if !exact && skipped > 0 {
                        skipped -= 1;
                        continue;
                    }
                    input = &input[next.len_utf8()..];
                }
            }
        }
    }
    fields.date(text, control)
}
