//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Custom parameters: the names a session may set without a definition, which `PostgreSQL` keeps as string placeholders (`assignable_custom_variable_name`).

use crate::SQLError;

/// Whether `name` is two or more simple identifiers separated by dots (`valid_custom_variable_name`). A component starts with a letter, an underscore or a byte of a multibyte character, and continues with those, digits and `$`.
pub fn valid_custom_name(name: &str) -> bool {
    let mut saw_separator = false;
    let mut component_start = true;
    for byte in name.bytes() {
        if byte == b'.' {
            if component_start {
                return false;
            }
            saw_separator = true;
            component_start = true;
        } else if byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80 {
            component_start = false;
        } else if component_start || !(byte.is_ascii_digit() || byte == b'$') {
            return false;
        }
    }
    !component_start && saw_separator
}

/// Check that a session may create a placeholder for `name`, which no definition names: a valid custom name outside every prefix that a loaded library reserves.
pub fn check_assignable_custom_name<'a>(
    name: &str,
    reserved_prefixes: impl IntoIterator<Item = &'a str>,
) -> Result<(), SQLError> {
    let Some((prefix, _)) = name.split_once('.') else {
        return Err(unrecognized_parameter(name));
    };
    if !valid_custom_name(name) {
        return Err(SQLError::Diagnostic {
            sqlstate: "42602".into(),
            message: format!("invalid configuration parameter name \"{name}\""),
            detail: Some(
                "Custom parameter names must be two or more simple identifiers separated by dots."
                    .into(),
            ),
            hint: None,
        });
    }
    if let Some(reserved) = reserved_prefixes
        .into_iter()
        .find(|reserved| *reserved == prefix)
    {
        return Err(SQLError::Diagnostic {
            sqlstate: "42602".into(),
            message: format!("invalid configuration parameter name \"{name}\""),
            detail: Some(format!("\"{reserved}\" is a reserved prefix.")),
            hint: None,
        });
    }
    Ok(())
}

/// The warning that removes a placeholder whose prefix a library reserves as it loads (`MarkGUCPrefixReserved`).
pub fn reserved_placeholder_warning(name: &str, prefix: &str) -> (String, String) {
    (
        format!("invalid configuration parameter name \"{name}\", removing it"),
        format!("\"{prefix}\" is now a reserved prefix."),
    )
}

/// `unrecognized configuration parameter "name"`, which `SHOW`, `RESET` of a single-part name and `current_setting` report for a name nothing defines.
pub fn unrecognized_parameter(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("unrecognized configuration parameter \"{name}\""),
    }
}

#[cfg(test)]
mod tests;
