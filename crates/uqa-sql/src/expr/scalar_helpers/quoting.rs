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

/// Double-quote unless the identifier is a safe lowercase name that is not a keyword.
pub fn quote_ident(ident: &str) -> String {
    quote_ident_with_control(ident, &ProductionControl::uncontrolled())
        .expect("ordinary identifier quoting")
        .into_uncontrolled()
        .expect("ordinary quoted identifier")
}

pub(in crate::expr) fn quote_ident_with_control(
    ident: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    control.check()?;
    let mut safe = !ident.is_empty();
    for (index, character) in ident.chars().enumerate() {
        control.check()?;
        safe &= character.is_ascii_lowercase()
            || character == '_'
            || (index > 0 && (character.is_ascii_digit() || character == '$'));
    }
    if safe && !super::is_quoted_keyword(ident) {
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
pub(in crate::expr) fn quote_literal_with_control(
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
