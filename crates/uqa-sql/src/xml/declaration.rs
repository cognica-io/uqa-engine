//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL/XML permits an XML declaration before content, independently of document parsing.

use uqa_core::{memory::ProductionControl, ValueRetentionError};

pub(super) enum Failure {
    Invalid(&'static str),
    Control(ValueRetentionError),
}

impl From<&'static str> for Failure {
    fn from(message: &'static str) -> Self {
        Self::Invalid(message)
    }
}
impl From<ValueRetentionError> for Failure {
    fn from(error: ValueRetentionError) -> Self {
        Self::Control(error)
    }
}

fn trim_space<'a>(
    input: &'a str,
    control: &ProductionControl<'_>,
) -> Result<&'a str, ValueRetentionError> {
    for (index, byte) in input.bytes().enumerate() {
        if index.is_multiple_of(4096) {
            control.check()?;
        }
        if !matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            return Ok(&input[index..]);
        }
    }
    Ok("")
}

fn find(
    input: &str,
    needle: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Option<usize>, ValueRetentionError> {
    for (index, window) in input.as_bytes().windows(needle.len()).enumerate() {
        if index.is_multiple_of(4096) {
            control.check()?;
        }
        if window == needle {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

#[derive(Default)]
pub(super) struct Declaration<'a> {
    pub length: usize,
    pub version: Option<&'a str>,
    pub standalone: Option<bool>,
}

fn space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

fn attribute<'a>(
    input: &mut &'a str,
    name: &str,
    failure: &'static str,
    control: &ProductionControl<'_>,
) -> Result<&'a str, Failure> {
    *input = trim_space(input.strip_prefix(name).ok_or(failure)?, control)?;
    *input = trim_space(input.strip_prefix('=').ok_or(failure)?, control)?;
    let quote = input
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"'))
        .ok_or(failure)?;
    let value = &input[1..];
    let length = find(value, &[quote as u8], control)?.ok_or(failure)?;
    *input = &value[length + 1..];
    Ok(&value[..length])
}

pub(super) fn parse<'a>(
    input: &'a str,
    control: &ProductionControl<'_>,
) -> Result<Declaration<'a>, Failure> {
    control.check()?;
    let Some(mut rest) = input.strip_prefix("<?xml") else {
        return Ok(Declaration::default());
    };
    if rest
        .chars()
        .next()
        .is_some_and(super::parser::is_name_character)
    {
        return Ok(Declaration::default());
    }
    if !rest.starts_with(space) {
        return Err("Space required.".into());
    }
    rest = trim_space(rest, control)?;
    let version = attribute(
        &mut rest,
        "version",
        "Malformed declaration: missing version.",
        control,
    )?;
    let after_space = trim_space(rest, control)?;
    if after_space.starts_with("encoding") {
        if after_space.len() == rest.len() {
            return Err("Space required.".into());
        }
        rest = after_space;
        attribute(
            &mut rest,
            "encoding",
            "Missing encoding in text declaration.",
            control,
        )?;
    }
    let after_space = trim_space(rest, control)?;
    let standalone = if after_space.starts_with("standalone") {
        if after_space.len() == rest.len() {
            return Err("Space required.".into());
        }
        rest = after_space;
        Some(
            match attribute(
                &mut rest,
                "standalone",
                "standalone accepts only 'yes' or 'no'.",
                control,
            )? {
                "yes" => true,
                "no" => false,
                _ => return Err("standalone accepts only 'yes' or 'no'.".into()),
            },
        )
    } else {
        None
    };
    rest = trim_space(rest, control)?
        .strip_prefix("?>")
        .ok_or("Parsing XML declaration: '?>' expected.")?;
    let length = input.len() - rest.len();
    for bytes in input.as_bytes()[..length].chunks(4096) {
        control.check()?;
        if !bytes.is_ascii() {
            return Err("Invalid character value.".into());
        }
    }
    Ok(Declaration {
        length,
        version: Some(version),
        standalone,
    })
}

pub(super) fn has_doctype(
    mut input: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, ValueRetentionError> {
    loop {
        input = trim_space(input, control)?;
        if input.starts_with("<!DOCTYPE") {
            return Ok(true);
        }
        if let Some(comment) = input.strip_prefix("<!--") {
            let Some(end) = find(comment, b"--", control)? else {
                return Ok(false);
            };
            let Some(rest) = comment[end..].strip_prefix("-->") else {
                return Ok(false);
            };
            input = rest;
        } else if let Some(instruction) = input.strip_prefix("<?") {
            let Some(end) = find(instruction, b"?>", control)? else {
                return Ok(false);
            };
            input = &instruction[end + 2..];
        } else {
            return Ok(false);
        }
    }
}
