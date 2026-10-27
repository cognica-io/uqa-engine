//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Calendar template output uses SQL's full date domain rather than a host date library.

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
const TOKENS: [&str; 20] = [
    "HH24", "HH12", "YYYY", "Month", "MONTH", "month", "DDD", "Mon", "MON", "mon", "Day", "DAY",
    "day", "Dy", "DY", "dy", "YY", "MM", "DD", "HH",
];

pub(super) fn format(
    value: TemporalDateTime,
    mut template: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut output = ProductionString::new(*control);
    let year = if value.year() > 0 {
        value.year()
    } else {
        1 - value.year()
    };
    while !template.is_empty() {
        control.check()?;
        if let Some(quoted) = template.strip_prefix('"') {
            template = quoted;
            while let Some(character) = template.chars().next() {
                control.check()?;
                template = &template[character.len_utf8()..];
                if character == '"' {
                    break;
                }
                output.push(character)?;
            }
            continue;
        }
        let fill = template.starts_with("FM");
        if fill {
            template = &template[2..];
        }
        let token = TOKENS
            .iter()
            .copied()
            .chain([
                "MI", "SS", "US", "MS", "AM", "PM", "am", "pm", "BC", "AD", "bc", "ad",
            ])
            .find(|token| template.starts_with(token));
        let Some(token) = token else {
            if let Some(character) = template.chars().next() {
                output.push(character)?;
                template = &template[character.len_utf8()..];
            }
            continue;
        };
        template = &template[token.len()..];
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
            "Month" | "MONTH" | "month" => MONTHS[(value.month() - 1) as usize],
            "Mon" | "MON" | "mon" => &MONTHS[(value.month() - 1) as usize][..3],
            "Day" | "DAY" | "day" => DAYS[value.date().weekday_sunday() as usize],
            "Dy" | "DY" | "dy" => &DAYS[value.date().weekday_sunday() as usize][..3],
            "AM" | "PM" | "am" | "pm" => {
                if value.hour() < 12 {
                    "AM"
                } else {
                    "PM"
                }
            }
            "BC" | "AD" | "bc" | "ad" => {
                if value.year() <= 0 {
                    "BC"
                } else {
                    "AD"
                }
            }
            _ => unreachable!("template token was selected"),
        };
        for character in text.chars() {
            let character = if token.bytes().all(|byte| byte.is_ascii_uppercase()) {
                character.to_ascii_uppercase()
            } else if token.bytes().all(|byte| byte.is_ascii_lowercase()) {
                character.to_ascii_lowercase()
            } else {
                character
            };
            output.push(character)?;
        }
        if !fill && matches!(token, "Month" | "MONTH" | "month" | "Day" | "DAY" | "day") {
            for _ in text.len()..9 {
                output.push(' ')?;
            }
        }
    }
    output.finish()
}

#[cfg(test)]
mod tests;
