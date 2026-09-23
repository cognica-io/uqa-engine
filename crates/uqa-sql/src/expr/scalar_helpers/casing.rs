//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Casing writes scalar mappings directly into their admitted destination.

use icu_casemap::CaseMapper;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};
use writeable::Writeable;

pub(in crate::expr) fn lowercase(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut output = ProductionString::new(*control);
    control.check()?;
    // Root-locale full mapping retains contextual final sigma, unlike character-by-character lowercasing. The shared owner uses the same Unicode data generation as the pinned Rust toolchain.
    let written = CaseMapper::new()
        .lowercase(
            text,
            &"und".parse().expect("valid root language identifier"),
        )
        .write_to(&mut output);
    let output = output.finish()?;
    assert!(
        written.is_ok(),
        "casing writer failed without a production error"
    );
    Ok(output)
}

pub(in crate::expr) fn casefold(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut output = ProductionString::new(*control);
    control.check()?;
    let written = CaseMapper::new().fold(text).write_to(&mut output);
    let output = output.finish()?;
    assert!(
        written.is_ok(),
        "casing writer failed without a production error"
    );
    Ok(output)
}

pub(in crate::expr) fn uppercase(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut output = ProductionString::new(*control);
    for character in text.chars() {
        control.check()?;
        for mapped in character.to_uppercase() {
            output.push(mapped)?;
        }
    }
    output.finish()
}

pub(in crate::expr) fn initcap(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut output = ProductionString::new(*control);
    let mut start = true;
    for character in text.chars() {
        control.check()?;
        if character.is_whitespace() {
            output.push(character)?;
            start = true;
        } else if start {
            for mapped in character.to_uppercase() {
                output.push(mapped)?;
            }
            start = false;
        } else {
            for mapped in character.to_lowercase() {
                output.push(mapped)?;
            }
        }
    }
    output.finish()
}

#[cfg(test)]
mod tests;
