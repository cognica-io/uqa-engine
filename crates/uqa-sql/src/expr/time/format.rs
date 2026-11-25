//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL date templates share field recognition across parsing and formatting.

use std::fmt::Write;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    TemporalDateTime, ValueRetentionError,
};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
mod input;
pub(in crate::expr) use input::parse_date;

const TOKENS: &[&str] = &[
    "HH24", "HH12", "YYYY", "MONTH", "DDD", "MON", "DAY", "DY", "YY", "MM", "DD", "HH", "MI", "SS",
    "US", "MS", "AM", "PM", "BC", "AD", "FX",
];

#[derive(Clone, Copy)]
enum Item<'a> {
    Field {
        name: &'static str,
        spelling: &'a str,
        fill: bool,
    },
    Literal {
        ch: char,
        quoted: bool,
    },
}

struct Template<'a> {
    rest: &'a str,
    quoted: bool,
}

impl<'a> Template<'a> {
    fn new(rest: &'a str) -> Self {
        Self {
            rest,
            quoted: false,
        }
    }

    fn field(s: &str) -> Option<(&'static str, &str, bool)> {
        let fill = s.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("FM"));
        let s = if fill { &s[2..] } else { s };
        TOKENS.iter().find_map(|name| {
            let spelling = s.get(..name.len())?;
            spelling
                .eq_ignore_ascii_case(name)
                .then_some((*name, spelling, fill))
        })
    }
    fn next(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Item<'a>>, ValueRetentionError> {
        loop {
            control.check()?;
            let Some(ch) = self.rest.chars().next() else {
                return Ok(None);
            };
            if ch == '"' {
                self.quoted = !self.quoted;
                self.rest = &self.rest[1..];
                continue;
            }
            if !self.quoted {
                if let Some((name, spelling, fill)) = Self::field(self.rest) {
                    self.rest = &self.rest[spelling.len() + if fill { 2 } else { 0 }..];
                    return Ok(Some(Item::Field {
                        name,
                        spelling,
                        fill,
                    }));
                }
            }
            self.rest = &self.rest[ch.len_utf8()..];
            if ch == '\\' && (self.quoted || self.rest.starts_with('"')) {
                let Some(escaped) = self.rest.chars().next() else {
                    return Ok(None);
                };
                self.rest = &self.rest[escaped.len_utf8()..];
                return Ok(Some(Item::Literal {
                    ch: escaped,
                    quoted: true,
                }));
            }
            return Ok(Some(Item::Literal {
                ch,
                quoted: self.quoted,
            }));
        }
    }
}

pub(super) fn format(
    value: TemporalDateTime,
    template: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut output = ProductionString::new(*control);
    let year = if value.year() > 0 {
        value.year()
    } else {
        1 - value.year()
    };
    let mut template = Template::new(template);
    while let Some(item) = template.next(control)? {
        let (token, spelling, fill) = match item {
            Item::Literal { ch, .. } => {
                output.push(ch)?;
                continue;
            }
            Item::Field { name: "FX", .. } => continue,
            Item::Field {
                name,
                spelling,
                fill,
            } => (name, spelling, fill),
        };
        let numeric = match token {
            "YYYY" => Some((i64::from(year), 4)),
            "YY" => Some((i64::from(year % 100), 2)),
            "MM" => Some((i64::from(value.month()), 2)),
            "DD" => Some((i64::from(value.day()), 2)),
            "DDD" => Some((i64::from(value.date().ordinal()), 3)),
            "HH24" => Some((i64::from(value.hour()), 2)),
            "HH12" | "HH" => Some((i64::from((value.hour() + 11) % 12 + 1), 2)),
            "MI" => Some((i64::from(value.minute()), 2)),
            "SS" => Some((i64::from(value.second()), 2)),
            "US" => Some((i64::from(value.subsecond_micros()), 6)),
            "MS" => Some((i64::from(value.subsecond_micros() / 1000), 3)),
            _ => None,
        };
        if let Some((number, width)) = numeric {
            let width = if fill { 0 } else { width };
            if write!(output, "{number:0width$}").is_err() {
                return output.finish();
            }
            continue;
        }
        let text = match token {
            "MONTH" => MONTHS[(value.month() - 1) as usize],
            "MON" => &MONTHS[(value.month() - 1) as usize][..3],
            "DAY" => DAYS[value.date().weekday_sunday() as usize],
            "DY" => &DAYS[value.date().weekday_sunday() as usize][..3],
            "AM" | "PM" => {
                if value.hour() < 12 {
                    "AM"
                } else {
                    "PM"
                }
            }
            "BC" | "AD" => {
                if value.year() <= 0 {
                    "BC"
                } else {
                    "AD"
                }
            }
            _ => unreachable!("template token was selected"),
        };
        for character in text.chars() {
            let character = if spelling.bytes().all(|byte| byte.is_ascii_uppercase()) {
                character.to_ascii_uppercase()
            } else if spelling.bytes().all(|byte| byte.is_ascii_lowercase()) {
                character.to_ascii_lowercase()
            } else {
                character
            };
            output.push(character)?;
        }
        if !fill && matches!(token, "MONTH" | "DAY") {
            for _ in text.len()..9 {
                output.push(' ')?;
            }
        }
    }
    output.finish()
}

#[cfg(test)]
mod tests;
