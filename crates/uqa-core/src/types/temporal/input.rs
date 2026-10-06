//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Date and time input as `PostgreSQL`'s input functions read it: ISO 8601 dates, times and timestamps with a UTC offset and an `AD` or `BC` era, the special values `now`, `today`, `tomorrow`, `yesterday`, `epoch` and `allballs`, and the diagnostics `DecodeDateTime`, `DecodeTimeOnly`, `DecodeTimezone` and `ValidateDate` report, each kept apart so the SQL layer names the type and the text.

use super::{epoch_date, TemporalValue, MICROS_PER_DAY, MICROS_PER_SECOND};
use chrono::NaiveDate;

/// Why date or time input was rejected, as `DateTimeParseError` distinguishes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemporalInputError {
    /// The text is not a value of the type (`22007`).
    InvalidSyntax,
    /// A field is outside its range (`22008`); `date_style` when the month or day field is, which `PostgreSQL` attributes to the `DateStyle` field order in its hint.
    FieldOverflow { date_style: bool },
    /// The value lies outside the type's range (`22008`).
    OutOfRange,
    /// A UTC offset past 15 hours, or with a minute or second field past its range (`22009`).
    ZoneDisplacement,
    /// A time zone name the input does not know, in lower case as the diagnostic prints it (`22023`).
    UnknownZone(String),
    /// An interval field does not fit its carrier (`22015`).
    IntervalFieldOverflow,
}

type Input<T> = Result<T, TemporalInputError>;

/// The reserved words `DecodeSpecial` recognizes, each a `DTK_*` value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Special {
    Now,
    Today,
    Tomorrow,
    Yesterday,
    Epoch,
    Allballs,
}

fn special(token: &str) -> Option<Special> {
    let lowered = token.to_ascii_lowercase();
    Some(match lowered.as_str() {
        "now" => Special::Now,
        "today" => Special::Today,
        "tomorrow" => Special::Tomorrow,
        "yesterday" => Special::Yesterday,
        "epoch" => Special::Epoch,
        "allballs" => Special::Allballs,
        _ => return None,
    })
}

/// A calendar date read from the text, validated as `ValidateDate` validates it.
#[derive(Clone, Copy)]
struct Calendar {
    year: i64,
    month: u32,
    day: u32,
}

#[derive(Clone, Copy)]
enum DatePart {
    Calendar(Calendar),
    Special(Special),
}

/// The fields one text supplies, before a type decides which it needs.
#[derive(Default)]
struct Fields {
    date: Option<DatePart>,
    /// Microseconds into the day, up to and including `24:00:00`.
    time: Option<i64>,
    /// The UTC offset in seconds east of Greenwich.
    zone: Option<i64>,
    meridian: Option<bool>,
    /// The era written after the fields: `Some(true)` for `BC`, which counts the year before the common era, `Some(false)` for `AD`.
    era: Option<bool>,
}

impl TemporalValue {
    /// `date_in`: a calendar date, or `now`, `today`, `tomorrow`, `yesterday` or `epoch` resolved against `now_micros`; a time of day or zone after the date is read and ignored.
    pub fn date_input(text: &str, now_micros: i64) -> Input<Self> {
        let fields = decode(text)?;
        let days = match fields.date {
            Some(DatePart::Calendar(calendar)) => calendar_days(calendar)?,
            Some(DatePart::Special(special)) => special_days(special, now_micros)?,
            None => return Err(TemporalInputError::InvalidSyntax),
        };
        Ok(Self::Date { days })
    }

    /// `time_in`: a time of day, or `now` or `allballs`; a date before it or a zone after it is read and ignored.
    pub fn time_input(text: &str, now_micros: i64) -> Input<Self> {
        let fields = decode(text)?;
        Ok(Self::Time {
            micros: time_of_day(&fields, now_micros)?,
        })
    }

    /// `timetz_in`: a time of day with its UTC offset, `+00` when the text names none, as the `UTC` session time zone supplies it.
    pub fn time_tz_input(text: &str, now_micros: i64) -> Input<Self> {
        let fields = decode(text)?;
        let micros = time_of_day(&fields, now_micros)?;
        let offset = fields.zone.unwrap_or(0);
        if offset % 60 != 0 {
            return Err(TemporalInputError::InvalidSyntax);
        }
        Ok(Self::TimeTz {
            micros,
            offset_minutes: i32::try_from(offset / 60)
                .map_err(|_| TemporalInputError::ZoneDisplacement)?,
        })
    }

    /// `timestamp_in`: a date with an optional time of day, or `now`, `today`, `tomorrow`, `yesterday` or `epoch`; a zone is read and ignored.
    pub fn timestamp_input(text: &str, now_micros: i64) -> Input<Self> {
        let fields = decode(text)?;
        let micros = instant(&fields, now_micros)?;
        if !Self::timestamp_micros_in_range(micros) {
            return Err(TemporalInputError::OutOfRange);
        }
        Ok(Self::Timestamp { micros })
    }

    /// `timestamptz_in`: as `timestamp_input`, with the UTC offset applied; a text without one is read in the `UTC` session time zone.
    pub fn timestamp_tz_input(text: &str, now_micros: i64) -> Input<Self> {
        let fields = decode(text)?;
        let local = instant(&fields, now_micros)?;
        let offset = fields.zone.unwrap_or(0) * MICROS_PER_SECOND;
        let micros = local
            .checked_sub(offset)
            .ok_or(TemporalInputError::OutOfRange)?;
        if !Self::timestamp_micros_in_range(micros) {
            return Err(TemporalInputError::OutOfRange);
        }
        Ok(Self::TimestampTz { micros })
    }
}

/// The time of day a `time` or `timetz` text supplies.
fn time_of_day(fields: &Fields, now_micros: i64) -> Input<i64> {
    match (fields.date, fields.time) {
        (Some(DatePart::Special(Special::Now)), None) => Ok(now_micros.rem_euclid(MICROS_PER_DAY)),
        (Some(DatePart::Special(Special::Allballs)), None) => Ok(0),
        // A calendar date before the time is read and ignored; any other special word, or a special word with a time, is no time.
        (Some(DatePart::Special(_)), _) | (_, None) => Err(TemporalInputError::InvalidSyntax),
        (Some(DatePart::Calendar(_)) | None, Some(micros)) => Ok(micros),
    }
}

/// The instant a `timestamp` text supplies, before any zone applies.
fn instant(fields: &Fields, now_micros: i64) -> Input<i64> {
    let days = match fields.date {
        Some(DatePart::Calendar(calendar)) => calendar_days(calendar)?,
        Some(DatePart::Special(Special::Now)) => {
            if fields.time.is_some() {
                return Err(TemporalInputError::InvalidSyntax);
            }
            return Ok(now_micros);
        }
        Some(DatePart::Special(Special::Epoch)) if fields.time.is_some() => {
            return Err(TemporalInputError::InvalidSyntax)
        }
        Some(DatePart::Special(special)) => special_days(special, now_micros)?,
        None => return Err(TemporalInputError::InvalidSyntax),
    };
    i64::from(days)
        .checked_mul(MICROS_PER_DAY)
        .and_then(|day| day.checked_add(fields.time.unwrap_or(0)))
        .ok_or(TemporalInputError::OutOfRange)
}

/// Days since 1970-01-01 of a special date value.
fn special_days(special: Special, now_micros: i64) -> Input<i32> {
    let today = now_micros.div_euclid(MICROS_PER_DAY);
    let days = match special {
        Special::Now | Special::Today => today,
        Special::Tomorrow => today + 1,
        Special::Yesterday => today - 1,
        Special::Epoch => 0,
        Special::Allballs => return Err(TemporalInputError::InvalidSyntax),
    };
    i32::try_from(days).map_err(|_| TemporalInputError::OutOfRange)
}

/// Days since 1970-01-01 of a validated calendar date; a year the day carrier cannot hold is out of range.
fn calendar_days(calendar: Calendar) -> Input<i32> {
    let year = i32::try_from(calendar.year).map_err(|_| TemporalInputError::OutOfRange)?;
    let date = NaiveDate::from_ymd_opt(year, calendar.month, calendar.day)
        .ok_or(TemporalInputError::OutOfRange)?;
    i32::try_from(date.signed_duration_since(epoch_date()).num_days())
        .map_err(|_| TemporalInputError::OutOfRange)
}

/// Split the text into its fields as `ParseDateTime` and `DecodeDateTime` do for the ISO forms: a date, a time of day with an optional glued offset, a separate offset or zone name, a meridian, or one special word.
fn decode(text: &str) -> Input<Fields> {
    let text = text.trim();
    if text.is_empty() {
        return Err(TemporalInputError::InvalidSyntax);
    }
    let mut fields = Fields::default();
    for token in text
        .split_whitespace()
        .flat_map(split_iso_separator)
        .flat_map(split_date_zone)
    {
        let token = token.trim_end_matches(',');
        if token.is_empty() {
            continue;
        }
        if let Some(special) = special(token) {
            if fields.date.is_some() || fields.time.is_some() {
                return Err(TemporalInputError::InvalidSyntax);
            }
            fields.date = Some(DatePart::Special(special));
            continue;
        }
        if token.starts_with(['+', '-']) {
            set_zone(&mut fields, decode_zone(token)?)?;
            continue;
        }
        if token.contains(':') {
            let (time, zone) = split_glued_zone(token);
            if fields.time.is_some() {
                return Err(TemporalInputError::InvalidSyntax);
            }
            fields.time = Some(decode_time(time)?);
            if let Some(zone) = zone {
                set_zone(&mut fields, decode_zone(zone)?)?;
            }
            continue;
        }
        if token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'/' | b'.'))
        {
            if fields.date.is_some() {
                return Err(TemporalInputError::InvalidSyntax);
            }
            fields.date = Some(DatePart::Calendar(decode_date(token)?));
            continue;
        }
        if token.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            match token.to_ascii_lowercase().as_str() {
                "am" | "pm" => {
                    if fields.meridian.is_some() {
                        return Err(TemporalInputError::InvalidSyntax);
                    }
                    fields.meridian = Some(token.eq_ignore_ascii_case("pm"));
                }
                // `DecodeDateTime` reads one era after the fields; `AD` leaves the year as written.
                "ad" | "bc" => {
                    if fields.era.is_some() {
                        return Err(TemporalInputError::InvalidSyntax);
                    }
                    fields.era = Some(token.eq_ignore_ascii_case("bc"));
                }
                _ => set_zone(&mut fields, decode_zone(token)?)?,
            }
            continue;
        }
        // A word with a slash is a time zone name, which the diagnostic names when the zone is unknown.
        if token.contains('/') && token.bytes().any(|byte| byte.is_ascii_alphabetic()) {
            return Err(TemporalInputError::UnknownZone(token.to_ascii_lowercase()));
        }
        return Err(TemporalInputError::InvalidSyntax);
    }
    if let Some(DatePart::Calendar(calendar)) = &mut fields.date {
        validate_date(calendar, fields.era == Some(true))?;
    } else if fields.era == Some(true) {
        return Err(TemporalInputError::InvalidSyntax);
    }
    if let Some(afternoon) = fields.meridian {
        let Some(micros) = fields.time else {
            return Err(TemporalInputError::InvalidSyntax);
        };
        let hour = micros / (3_600 * MICROS_PER_SECOND);
        // `DecodeDateTime` admits 1 through 12 with a meridian and moves 12 to the start of its half.
        if !(1..=12).contains(&hour) {
            return Err(TemporalInputError::FieldOverflow { date_style: false });
        }
        let shift = match (afternoon, hour) {
            (true, 12) | (false, 1..=11) => 0,
            (true, _) => 12,
            (false, _) => -12,
        };
        fields.time = Some(micros + shift * 3_600 * MICROS_PER_SECOND);
    }
    Ok(fields)
}

/// Keep `2024-01-01T10:00:00` as a date and a time: the ISO `T` separates two numeric fields.
fn split_iso_separator(token: &str) -> impl Iterator<Item = &str> {
    let split = token.bytes().enumerate().find_map(|(index, byte)| {
        (matches!(byte, b'T' | b't')
            && index > 0
            && token.as_bytes()[index - 1].is_ascii_digit()
            && token
                .as_bytes()
                .get(index + 1)
                .is_some_and(u8::is_ascii_digit))
        .then_some(index)
    });
    match split {
        Some(index) => [Some(&token[..index]), Some(&token[index + 1..])],
        None => [Some(token), None],
    }
    .into_iter()
    .flatten()
}

/// `ParseDateTime` consumes a numeric date's matching delimiters before starting a zone field. A trailing minus stays in a hyphenated date, whereas a plus (or a minus after a slash/dot date) starts a separate offset even without whitespace.
fn split_date_zone(token: &str) -> impl Iterator<Item = &str> {
    let digits = token.bytes().take_while(u8::is_ascii_digit).count();
    let end = match token.as_bytes().get(digits) {
        Some(delimiter @ (b'-' | b'/' | b'.')) if digits > 0 => token
            .bytes()
            .take_while(|byte| byte.is_ascii_digit() || byte == delimiter)
            .count(),
        _ => digits,
    };
    let split = (end > 0 && end < token.len())
        .then(|| token.split_at(end))
        .filter(|(_, zone)| zone.starts_with(['+', '-']) || zone.eq_ignore_ascii_case("z"));
    match split {
        Some((date, zone)) => [Some(date), Some(zone)],
        None => [Some(token), None],
    }
    .into_iter()
    .flatten()
}

/// Split `10:00:00+02`, `10:00:00-05:30` or `10:00Z` into the time and its offset.
fn split_glued_zone(token: &str) -> (&str, Option<&str>) {
    if let Some(time) = token.strip_suffix('Z').or_else(|| token.strip_suffix('z')) {
        return (time, Some("z"));
    }
    match token.rfind(['+', '-']) {
        Some(index) if index > 0 => (&token[..index], Some(&token[index..])),
        _ => (token, None),
    }
}

fn set_zone(fields: &mut Fields, zone: i64) -> Input<()> {
    if fields.zone.is_some() {
        return Err(TemporalInputError::InvalidSyntax);
    }
    fields.zone = Some(zone);
    Ok(())
}

/// Read `YYYY-MM-DD`, `YYYY/MM/DD`, `YYYY.MM.DD`, `YYYYMMDD` and `YYMMDD`, and a first field of fewer than three digits in the `MDY` order of the default `DateStyle`. Calendar validity follows the remaining field decoding, as in `ValidateDate`.
fn decode_date(token: &str) -> Input<Calendar> {
    let parts: Vec<&str> = token.split(['-', '/', '.']).collect();
    let (year, month, day) = match parts.as_slice() {
        [compact] => match compact.len() {
            8 => (
                number(&compact[..4])?,
                number(&compact[4..6])?,
                number(&compact[6..])?,
            ),
            6 => (
                two_digit_year(number(&compact[..2])?),
                number(&compact[2..4])?,
                number(&compact[4..])?,
            ),
            _ => return Err(TemporalInputError::InvalidSyntax),
        },
        [first, second, third] => {
            if first.len() >= 3 {
                (number(first)?, number(second)?, number(third)?)
            } else {
                let year = number(third)?;
                let year = if third.len() <= 2 {
                    two_digit_year(year)
                } else {
                    year
                };
                (year, number(first)?, number(second)?)
            }
        }
        _ => return Err(TemporalInputError::InvalidSyntax),
    };
    let (Ok(month), Ok(day)) = (u32::try_from(month), u32::try_from(day)) else {
        return Err(TemporalInputError::FieldOverflow { date_style: true });
    };
    Ok(Calendar { year, month, day })
}

/// `ValidateDate` runs after zone decoding and applies the era before checking leap days; 1 BC is the proleptic year zero, and the written year zero is never valid.
fn validate_date(calendar: &mut Calendar, bc: bool) -> Input<()> {
    if calendar.year <= 0 {
        return Err(TemporalInputError::FieldOverflow { date_style: false });
    }
    if bc {
        calendar.year = 1 - calendar.year;
    }
    if !(1..=12).contains(&calendar.month) || !(1..=31).contains(&calendar.day) {
        return Err(TemporalInputError::FieldOverflow { date_style: true });
    }
    if calendar.day > days_in_month(calendar.year, calendar.month) {
        return Err(TemporalInputError::FieldOverflow { date_style: false });
    }
    Ok(())
}

/// `DecodeDate` reads a two-digit year as 1970 through 2069.
fn two_digit_year(year: i64) -> i64 {
    if year < 70 {
        year + 2000
    } else {
        year + 1900
    }
}

fn number(text: &str) -> Input<i64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TemporalInputError::InvalidSyntax);
    }
    text.parse().map_err(|_| TemporalInputError::OutOfRange)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
    }
}

/// Read `HH:MM`, `HH:MM:SS` and `HH:MM:SS.ffffff`, rounding past the sixth fractional digit as `ParseFractionalSecond` does, and validate as `DecodeTimeOnly` does: minutes up to 59, seconds up to 60 (a sixtieth second carries into the next minute), hours up to 24 with `24:00:00` the only value of its hour.
fn decode_time(token: &str) -> Input<i64> {
    let mut parts = token.split(':');
    let hour = number(parts.next().unwrap_or_default())?;
    let minute = number(parts.next().ok_or(TemporalInputError::InvalidSyntax)?)?;
    let (second, fraction) = match parts.next() {
        Some(second) => {
            let (whole, fraction) = second.split_once('.').unwrap_or((second, ""));
            (number(whole)?, fraction_micros(fraction)?)
        }
        None => (0, 0),
    };
    if parts.next().is_some() {
        return Err(TemporalInputError::InvalidSyntax);
    }
    if !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
        || !(0..=24).contains(&hour)
        || (hour == 24 && (minute > 0 || second > 0 || fraction > 0))
    {
        return Err(TemporalInputError::FieldOverflow { date_style: false });
    }
    Ok(((hour * 60 + minute) * 60 + second) * MICROS_PER_SECOND + fraction)
}

/// Microseconds of a fractional-second digit string, rounded half up past six digits.
fn fraction_micros(digits: &str) -> Input<i64> {
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TemporalInputError::InvalidSyntax);
    }
    let mut micros = 0_i64;
    for (index, digit) in digits.bytes().enumerate() {
        let digit = i64::from(digit - b'0');
        if index < 6 {
            micros = micros * 10 + digit;
        } else if index == 6 {
            micros += i64::from(digit >= 5);
            break;
        }
    }
    let scale = 10_i64.pow(6_u32.saturating_sub(u32::try_from(digits.len()).unwrap_or(6)));
    Ok(micros * scale)
}

/// Read a UTC offset or zone abbreviation as `DecodeTimezone` and the abbreviation table do: `+HH`, `+HH:MM`, `+HHMM`, `+HH:MM:SS`, `Z`, `UTC` and `GMT`; an offset past 15 hours or with a minute or second field past 59 is a displacement error, and a word that is no abbreviation is a syntax error, as `DecodeSpecial` reports an unknown word.
fn decode_zone(token: &str) -> Input<i64> {
    if token.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return match token.to_ascii_lowercase().as_str() {
            "z" | "zulu" | "utc" | "gmt" | "ut" => Ok(0),
            _ => Err(TemporalInputError::InvalidSyntax),
        };
    }
    let sign = match token.as_bytes().first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return Err(TemporalInputError::InvalidSyntax),
    };
    let body = &token[1..];
    let zone_number = |text| {
        number(text).map_err(|error| match error {
            TemporalInputError::OutOfRange => TemporalInputError::ZoneDisplacement,
            other => other,
        })
    };
    let (hours, minutes, seconds) = if body.contains(':') {
        let mut parts = body.split(':');
        let hours = zone_number(parts.next().unwrap_or_default())?;
        let minutes = zone_number(parts.next().filter(|part| !part.is_empty()).unwrap_or("0"))?;
        let seconds = zone_number(parts.next().filter(|part| !part.is_empty()).unwrap_or("0"))?;
        if parts.next().is_some() {
            return Err(TemporalInputError::InvalidSyntax);
        }
        (hours, minutes, seconds)
    } else {
        let number = zone_number(body)?;
        if body.len() > 2 {
            (number / 100, number % 100, 0)
        } else {
            (number, 0, 0)
        }
    };
    if !(0..=59).contains(&minutes) || !(0..=59).contains(&seconds) || hours > 15 {
        return Err(TemporalInputError::ZoneDisplacement);
    }
    Ok(sign * ((hours * 60 + minutes) * 60 + seconds))
}

#[cfg(test)]
mod tests;
