//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source bodies, estimates and saved configuration in routine definitions.

use crate::semantics::parameters::{catalog::find_parameter, definition::ParameterFlags};
use crate::SQLError;

/// Dollar-quote a source body as `PostgreSQL` does, extending the delimiter when its prefix occurs anywhere in the unchanged source. The caller supplies the final newline.
pub fn source_clause(source: &str, procedure: bool) -> String {
    let mut delimiter = if procedure {
        "$procedure".to_owned()
    } else {
        "$function".to_owned()
    };
    while source.contains(delimiter.as_str()) {
        delimiter.push('x');
    }
    delimiter.push('$');
    format!("AS {delimiter}{source}{delimiter}")
}

/// Render a catalog `float4` estimate with the six significant digits of `PostgreSQL`'s `%g`, including its exponent spelling and special values.
pub fn estimate(value: f32) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if value.is_infinite() {
        return format!("{sign}Infinity");
    }
    if value == 0.0 {
        return format!("{sign}0");
    }

    // C promotes the stored float4 to double before formatting. Round once to
    // six significant digits, then place the decimal point without rounding.
    let scientific = format!("{:.5e}", f64::from(value.abs()));
    let (mantissa, exponent) = scientific.split_once('e').expect("scientific estimate");
    let exponent: i32 = exponent.parse().expect("scientific exponent");
    if !(-4..6).contains(&exponent) {
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        return format!("{sign}{mantissa}e{exponent:+03}");
    }

    let digits = mantissa.replace('.', "");
    let mut output = if exponent < 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-exponent - 1) as usize))
    } else {
        let (integer, fraction) = digits.split_at(exponent as usize + 1);
        if fraction.is_empty() {
            return format!("{sign}{integer}");
        }
        format!("{sign}{integer}.{fraction}")
    };
    while output.ends_with('0') {
        output.pop();
    }
    if output.ends_with('.') {
        output.pop();
    }
    output
}

/// Render ordered saved `SET` clauses, each with its leading space and trailing newline. Quoted-list parameters preserve element case and length; string literals follow `standard_conforming_strings` without adding an `E` prefix.
pub fn configuration_clauses(
    config: &[(String, String)],
    standard_strings: bool,
) -> Result<String, SQLError> {
    let mut output = String::new();
    for (name, value) in config {
        output.push_str(" SET ");
        output.push_str(&crate::expr::quote_ident(name));
        output.push_str(" TO ");
        if find_parameter(name)
            .is_some_and(|definition| definition.has_flag(ParameterFlags::LIST_QUOTE))
        {
            let values = configuration_list(value).ok_or_else(|| SQLError::Routine {
                sqlstate: "XX000".into(),
                message: "invalid list syntax in proconfig item".into(),
            })?;
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push_str(", ");
                }
                append_literal(&mut output, value, standard_strings);
            }
        } else {
            append_literal(&mut output, value, standard_strings);
        }
        output.push('\n');
    }
    Ok(output)
}

fn append_literal(output: &mut String, value: &str, standard_strings: bool) {
    output.push('\'');
    for character in value.chars() {
        if character == '\'' || (character == '\\' && !standard_strings) {
            output.push(character);
        }
        output.push(character);
    }
    output.push('\'');
}

// SplitGUCList does not normalize identifiers: saved list elements can also be
// empty strings or paths longer than an identifier. Its whitespace is the SQL
// scanner's six ASCII whitespace characters, independent of the locale.
fn configuration_list(value: &str) -> Option<Vec<String>> {
    let mut input = value.chars().peekable();
    let mut values = Vec::new();
    while input.peek().copied().is_some_and(list_space) {
        input.next();
    }
    if input.peek().is_none() {
        return Some(values);
    }
    loop {
        let mut value = String::new();
        if input.peek() == Some(&'"') {
            input.next();
            loop {
                match input.next()? {
                    '"' if input.peek() == Some(&'"') => {
                        input.next();
                        value.push('"');
                    }
                    '"' => break,
                    character => value.push(character),
                }
            }
        } else {
            while input
                .peek()
                .copied()
                .is_some_and(|character| character != ',' && !list_space(character))
            {
                value.push(input.next()?);
            }
            if value.is_empty() {
                return None;
            }
        }
        while input.peek().copied().is_some_and(list_space) {
            input.next();
        }
        values.push(value);
        match input.next() {
            None => return Some(values),
            Some(',') => {
                while input.peek().copied().is_some_and(list_space) {
                    input.next();
                }
            }
            Some(_) => return None,
        }
    }
}

const fn list_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}')
}

#[cfg(test)]
mod tests;
