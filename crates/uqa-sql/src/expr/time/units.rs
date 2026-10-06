//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` temporal unit tokens shared by truncation and extraction.

use crate::error::SQLError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unit {
    Millennium,
    Century,
    Decade,
    Year,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Milliseconds,
    Microseconds,
    Timezone,
    TimezoneHour,
    TimezoneMinute,
    Dow,
    IsoDow,
    Doy,
    IsoYear,
    Julian,
    Epoch,
    Reserved,
}

pub(super) struct UnitName {
    bytes: [u8; 63],
    len: usize,
}

impl UnitName {
    pub(super) fn new(raw: &str) -> Self {
        // `downcase_truncate_identifier` clips on a UTF-8 boundary and folds ASCII case, without trimming whitespace.
        let mut len = raw.len().min(63);
        while !raw.is_char_boundary(len) {
            len -= 1;
        }
        let mut bytes = [0_u8; 63];
        for (output, input) in bytes.iter_mut().zip(&raw.as_bytes()[..len]) {
            *output = input.to_ascii_lowercase();
        }
        Self { bytes, len }
    }

    fn name(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len])
            .expect("ASCII folding and character-boundary truncation preserve UTF-8")
    }

    fn token(&self) -> &[u8] {
        &self.bytes[..self.len.min(10)]
    }

    /// The `DecodeUnits` table compares at most ten bytes of the normalized name.
    pub(super) fn units(&self) -> Option<Unit> {
        Some(match self.token() {
            b"mil" | b"mils" | b"millennia" | b"millennium" => Unit::Millennium,
            b"c" | b"cent" | b"century" | b"centuries" => Unit::Century,
            b"dec" | b"decs" | b"decade" | b"decades" => Unit::Decade,
            b"y" | b"yr" | b"yrs" | b"year" | b"years" => Unit::Year,
            b"qtr" | b"quarter" => Unit::Quarter,
            b"mon" | b"mons" | b"month" | b"months" => Unit::Month,
            b"w" | b"week" | b"weeks" => Unit::Week,
            b"d" | b"day" | b"days" => Unit::Day,
            b"h" | b"hr" | b"hrs" | b"hour" | b"hours" => Unit::Hour,
            b"m" | b"min" | b"mins" | b"minute" | b"minutes" => Unit::Minute,
            b"s" | b"sec" | b"secs" | b"second" | b"seconds" => Unit::Second,
            b"ms" | b"msec" | b"msecs" | b"msecond" | b"mseconds" | b"millisecon" => {
                Unit::Milliseconds
            }
            b"us" | b"usec" | b"usecs" | b"usecond" | b"useconds" | b"microsecon" => {
                Unit::Microseconds
            }
            b"timezone" => Unit::Timezone,
            b"timezone_h" => Unit::TimezoneHour,
            b"timezone_m" => Unit::TimezoneMinute,
            _ => return None,
        })
    }

    /// Extraction falls back to `DecodeSpecial`; truncation does not.
    pub(super) fn extraction(&self) -> Option<Unit> {
        self.units().or_else(|| {
            Some(match self.token() {
                b"dow" => Unit::Dow,
                b"isodow" => Unit::IsoDow,
                b"doy" => Unit::Doy,
                b"isoyear" => Unit::IsoYear,
                b"j" | b"jd" | b"julian" => Unit::Julian,
                b"mm" => Unit::Minute,
                b"epoch" => Unit::Epoch,
                b"+infinity" | b"-infinity" | b"infinity" | b"allballs" | b"now" | b"today"
                | b"tomorrow" | b"yesterday" => Unit::Reserved,
                _ => return None,
            })
        })
    }

    pub(super) fn unsupported(&self, type_name: &str, detail: Option<&str>) -> SQLError {
        SQLError::Diagnostic {
            sqlstate: "0A000".into(),
            message: format!(
                "unit \"{}\" not supported for type {type_name}",
                self.name()
            ),
            detail: detail.map(str::to_string),
            hint: None,
        }
    }

    pub(super) fn unrecognized(&self, type_name: &str) -> SQLError {
        SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!(
                "unit \"{}\" not recognized for type {type_name}",
                self.name()
            ),
        }
    }
}
