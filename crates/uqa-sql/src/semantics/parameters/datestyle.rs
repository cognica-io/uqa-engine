//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DateStyle` input order and canonical assignments, including partial settings and reset defaults.

use super::{definition::ParameterDefinition, identifier_list::split_identifier_list};
use crate::SQLError;
use uqa_core::TemporalDateOrder;

#[derive(Clone, Copy, Default)]
struct DateStyle {
    style: Style,
    order: TemporalDateOrder,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Style {
    #[default]
    Iso,
    Sql,
    Postgres,
    German,
}

impl DateStyle {
    fn canonical(setting: &str) -> Self {
        let mut value = Self::default();
        for token in setting.split(',').map(str::trim) {
            match token {
                "ISO" => value.style = Style::Iso,
                "SQL" => value.style = Style::Sql,
                "Postgres" => value.style = Style::Postgres,
                "German" => value.style = Style::German,
                "MDY" => value.order = TemporalDateOrder::MonthDayYear,
                "DMY" => value.order = TemporalDateOrder::DayMonthYear,
                "YMD" => value.order = TemporalDateOrder::YearMonthDay,
                _ => {}
            }
        }
        value
    }

    fn display(self) -> String {
        let style = match self.style {
            Style::Iso => "ISO",
            Style::Sql => "SQL",
            Style::Postgres => "Postgres",
            Style::German => "German",
        };
        let order = match self.order {
            TemporalDateOrder::MonthDayYear => "MDY",
            TemporalDateOrder::DayMonthYear => "DMY",
            TemporalDateOrder::YearMonthDay => "YMD",
        };
        format!("{style}, {order}")
    }
}

/// The date order in a validated, canonical session setting.
#[must_use]
pub fn date_order(setting: &str) -> TemporalDateOrder {
    DateStyle::canonical(setting).order
}

pub(super) fn parse_setting(
    definition: &ParameterDefinition,
    raw: &str,
    current: &str,
    reset: &str,
) -> Result<String, SQLError> {
    let error = |detail: String| SQLError::Diagnostic {
        sqlstate: "22023".into(),
        message: super::value::invalid_value_message(definition, raw),
        detail: Some(detail),
        hint: None,
    };
    let tokens =
        split_identifier_list(raw, b',').ok_or_else(|| error("List syntax is invalid.".into()))?;
    let mut value = DateStyle::canonical(current);
    let reset = DateStyle::canonical(reset);
    let mut have_style = false;
    let mut have_order = false;
    let mut conflict = false;
    for token in tokens {
        let lowered = token.to_ascii_lowercase();
        let style = match lowered.as_str() {
            "iso" => Some(Style::Iso),
            "sql" => Some(Style::Sql),
            "german" => Some(Style::German),
            token if token.starts_with("postgres") => Some(Style::Postgres),
            _ => None,
        };
        if let Some(style) = style {
            conflict |= have_style && value.style != style;
            value.style = style;
            have_style = true;
            if style == Style::German && !have_order {
                value.order = TemporalDateOrder::DayMonthYear;
            }
            continue;
        }
        let order = match lowered.as_str() {
            "ymd" => Some(TemporalDateOrder::YearMonthDay),
            "dmy" => Some(TemporalDateOrder::DayMonthYear),
            "mdy" | "us" => Some(TemporalDateOrder::MonthDayYear),
            token if token.starts_with("euro") => Some(TemporalDateOrder::DayMonthYear),
            token if token.starts_with("noneuro") => Some(TemporalDateOrder::MonthDayYear),
            _ => None,
        };
        if let Some(order) = order {
            conflict |= have_order && value.order != order;
            value.order = order;
            have_order = true;
        } else if lowered == "default" {
            if !have_style {
                value.style = reset.style;
            }
            if !have_order {
                value.order = reset.order;
            }
        } else {
            return Err(error(format!("Unrecognized key word: \"{token}\".")));
        }
    }
    if conflict {
        return Err(error("Conflicting \"DateStyle\" specifications.".into()));
    }
    Ok(value.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignments_preserve_unspecified_fields_and_use_reset_defaults() {
        let definition = super::super::catalog::find_parameter("DateStyle").unwrap();
        for (raw, current, reset, expected) in [
            ("ISO", "SQL, YMD", "ISO, MDY", "ISO, YMD"),
            ("German", "ISO, MDY", "ISO, MDY", "German, DMY"),
            ("US, German", "ISO, YMD", "ISO, MDY", "German, MDY"),
            ("DEFAULT, YMD", "SQL, MDY", "German, DMY", "German, YMD"),
            ("SQL, DEFAULT", "ISO, MDY", "German, DMY", "SQL, DMY"),
            ("european", "ISO, MDY", "ISO, MDY", "ISO, DMY"),
            ("", "ISO, YMD", "ISO, MDY", "ISO, YMD"),
        ] {
            assert_eq!(
                parse_setting(definition, raw, current, reset).unwrap(),
                expected
            );
        }
        for (raw, detail) in [
            ("MDY,DMY", "Conflicting \"DateStyle\" specifications."),
            ("ISO,", "List syntax is invalid."),
            ("bogus", "Unrecognized key word: \"bogus\"."),
        ] {
            let error = parse_setting(definition, raw, "ISO, MDY", "ISO, MDY").unwrap_err();
            assert_eq!(error.sqlstate(), Some("22023"));
            assert_eq!(error.detail(), Some(detail));
        }
    }
}
