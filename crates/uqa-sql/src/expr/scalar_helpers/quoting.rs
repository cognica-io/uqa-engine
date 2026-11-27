//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL quoting scans borrowed input and admits escaped output through the shared string owner.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

/// Catalog definitions follow the current string setting and never emit an `E` prefix (`simple_quote_literal`).
pub(crate) fn append_sql_literal(output: &mut String, value: &str, standard_strings: bool) {
    output.push('\'');
    for character in value.chars() {
        if character == '\'' || (character == '\\' && !standard_strings) {
            output.push(character);
        }
        output.push(character);
    }
    output.push('\'');
}

/// Double-quote unless the identifier starts with a lowercase letter or an underscore, holds only lowercase letters, digits and underscores, and is not a keyword other than an unreserved one (`quote_identifier`).
pub fn quote_ident(ident: &str) -> String {
    quote_ident_with_control(ident, &ProductionControl::uncontrolled())
        .expect("ordinary identifier quoting")
        .into_uncontrolled()
        .expect("ordinary quoted identifier")
}

pub fn quote_ident_with_control(
    ident: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    render_identifier(ident, identifier_quotes(ident, control)?, control)
}

pub(crate) fn quote_sql_identifier(
    ident: &str,
    control: &ProductionControl<'_>,
) -> crate::error::Result<Produced<String>> {
    let quotes = identifier_quotes(ident, control)?;
    if let Some(quotes) = quotes {
        crate::expr::text_size::check_allocation_size(ident.len() as u128 + quotes as u128 + 3)?;
    }
    Ok(render_identifier(ident, quotes, control)?)
}

fn identifier_quotes(
    ident: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<usize>, ValueRetentionError> {
    control.check()?;
    let mut safe = !ident.is_empty();
    let mut quotes = 0;
    for (index, character) in ident.chars().enumerate() {
        control.check()?;
        safe &= character.is_ascii_lowercase()
            || character == '_'
            || (index > 0 && character.is_ascii_digit());
        quotes += usize::from(character == '"');
    }
    Ok((!safe || super::is_quoted_keyword(ident)).then_some(quotes))
}

fn render_identifier(
    ident: &str,
    quotes: Option<usize>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    if quotes.is_none() {
        return control.copy_text(ident);
    }
    let mut output = ProductionString::new(*control);
    output.push('"')?;
    for character in ident.chars() {
        if character == '"' {
            output.push('"')?;
        }
        output.push(character)?;
    }
    output.push('"')?;
    output.finish()
}

/// Single-quote with doubled quotes; backslashes select the escaped-literal form.
pub fn quote_literal_with_control(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    control.check()?;
    let mut escape = false;
    for character in text.chars() {
        control.check()?;
        escape |= character == '\\';
    }
    let mut output = ProductionString::new(*control);
    if escape {
        output.push('E')?;
    }
    output.push('\'')?;
    for character in text.chars() {
        if matches!(character, '\'' | '\\') {
            output.push(character)?;
        }
        output.push(character)?;
    }
    output.push('\'')?;
    output.finish()
}
