//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parameter values read and shown as `PostgreSQL`'s `parse_and_validate_value` and `ShowGUCOption` do. A setting is kept as the text `pg_settings.setting` reports: an integer in base units, `on` or `off`, the canonical name of an enumerated value, or the string itself.

use super::definition::{EnumOption, ParameterDefinition, ParameterFlags, ParameterKind};
use super::units::{parse_integer, show_integer};
use crate::SQLError;

/// The longest identifier, `NAMEDATALEN - 1` bytes, to which a `GUC_IS_NAME` string is truncated.
const MAX_IDENTIFIER_BYTES: usize = 63;

/// Read `raw` as a value of `definition` and return the setting it stands for, or the error `SET` reports for it.
pub fn parse_setting(definition: &ParameterDefinition, raw: &str) -> Result<String, SQLError> {
    match definition.kind {
        ParameterKind::Bool { .. } => parse_bool(raw)
            .map(|value| if value { "on" } else { "off" }.to_string())
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "22023".into(),
                message: format!("parameter \"{}\" requires a Boolean value", definition.name),
            }),
        ParameterKind::Integer { min, max, unit, .. } => {
            let value = parse_integer(raw, unit).map_err(|hint| SQLError::Diagnostic {
                sqlstate: "22023".into(),
                message: invalid_value_message(definition, raw),
                detail: None,
                hint: hint.map(str::to_string),
            })?;
            if value < min || value > max {
                let unit = unit.map_or(String::new(), |unit| format!(" {}", unit.name()));
                return Err(SQLError::Routine {
                    sqlstate: "22023".into(),
                    message: format!(
                        "{value}{unit} is outside the valid range for parameter \"{}\" ({min}{unit} .. {max}{unit})",
                        definition.name
                    ),
                });
            }
            Ok(value.to_string())
        }
        ParameterKind::Enum { options, .. } => options
            .iter()
            .find(|option| option.name.eq_ignore_ascii_case(raw))
            .map(|option| enum_display(options, option.value).to_string())
            .ok_or_else(|| SQLError::Diagnostic {
                sqlstate: "22023".into(),
                message: invalid_value_message(definition, raw),
                detail: None,
                hint: Some(format!(
                    "Available values: {}.",
                    listed_enum_values(options).join(", ")
                )),
            }),
        ParameterKind::String { .. } => Ok(if definition.has_flag(ParameterFlags::IS_NAME) {
            truncate_identifier(raw).to_string()
        } else {
            raw.to_string()
        }),
    }
}

/// The notice `PostgreSQL` raises before it truncates a `GUC_IS_NAME` string that is longer than an identifier.
pub fn name_truncation_notice(definition: &ParameterDefinition, raw: &str) -> Option<String> {
    let truncated = truncate_identifier(raw);
    (matches!(definition.kind, ParameterKind::String { .. })
        && definition.has_flag(ParameterFlags::IS_NAME)
        && truncated.len() < raw.len())
    .then(|| format!("identifier \"{raw}\" will be truncated to \"{truncated}\""))
}

/// `invalid value for parameter "name": "value"`, the message of a value that does not parse or that a check rejects.
pub fn invalid_value_message(definition: &ParameterDefinition, raw: &str) -> String {
    format!(
        "invalid value for parameter \"{}\": \"{raw}\"",
        definition.name
    )
}

/// Show `setting` as `SHOW` and `current_setting` report it: an integer in the greatest unit that divides it.
pub fn show_setting(definition: &ParameterDefinition, setting: &str) -> String {
    match definition.kind {
        ParameterKind::Integer { unit, .. } => setting
            .parse::<i64>()
            .map_or_else(|_| setting.to_string(), |value| show_integer(value, unit)),
        ParameterKind::Bool { .. } | ParameterKind::Enum { .. } | ParameterKind::String { .. } => {
            setting.to_string()
        }
    }
}

/// The names of an enumerated parameter's values that `pg_settings.enumvals` lists.
pub fn listed_enum_values(options: &[EnumOption]) -> Vec<&'static str> {
    options
        .iter()
        .filter(|option| !option.hidden)
        .map(|option| option.name)
        .collect()
}

/// The name an enumerated value displays as: the first option with that value.
pub(super) fn enum_display(options: &[EnumOption], value: u8) -> &'static str {
    options
        .iter()
        .find(|option| option.value == value)
        .map(|option| option.name)
        .expect("enumerated value has a name")
}

/// `parse_bool`: a prefix of `true`, `false`, `yes` or `no`, at least two letters of `on` or `off`, or `1` or `0`, in any case.
fn parse_bool(raw: &str) -> Option<bool> {
    let lower = raw.to_ascii_lowercase();
    let prefix_of = |word: &str| !lower.is_empty() && word.starts_with(lower.as_str());
    match lower.as_bytes().first()? {
        b't' if prefix_of("true") => Some(true),
        b'f' if prefix_of("false") => Some(false),
        b'y' if prefix_of("yes") => Some(true),
        b'n' if prefix_of("no") => Some(false),
        b'o' if lower.len() >= 2 && prefix_of("on") => Some(true),
        b'o' if lower.len() >= 2 && prefix_of("off") => Some(false),
        b'1' if lower.len() == 1 => Some(true),
        b'0' if lower.len() == 1 => Some(false),
        _ => None,
    }
}

/// `truncate_identifier`: the longest prefix of at most `NAMEDATALEN - 1` bytes that ends on a character boundary.
fn truncate_identifier(raw: &str) -> &str {
    if raw.len() <= MAX_IDENTIFIER_BYTES {
        return raw;
    }
    let mut end = MAX_IDENTIFIER_BYTES;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    &raw[..end]
}

#[cfg(test)]
mod tests;
