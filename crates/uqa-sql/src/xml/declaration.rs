//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL/XML permits an XML declaration before content, independently of document parsing.

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
) -> Result<&'a str, &'static str> {
    *input = input
        .strip_prefix(name)
        .ok_or(failure)?
        .trim_start_matches(space);
    *input = input
        .strip_prefix('=')
        .ok_or(failure)?
        .trim_start_matches(space);
    let quote = input
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"'))
        .ok_or(failure)?;
    let value = &input[1..];
    let length = value.find(quote).ok_or(failure)?;
    *input = &value[length + 1..];
    Ok(&value[..length])
}

pub(super) fn parse(input: &str) -> Result<Declaration<'_>, &'static str> {
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
        return Err("Space required.");
    }
    rest = rest.trim_start_matches(space);
    let version = attribute(
        &mut rest,
        "version",
        "Malformed declaration: missing version.",
    )?;
    let after_space = rest.trim_start_matches(space);
    if after_space.starts_with("encoding") {
        if after_space.len() == rest.len() {
            return Err("Space required.");
        }
        rest = after_space;
        attribute(
            &mut rest,
            "encoding",
            "Missing encoding in text declaration.",
        )?;
    }
    let after_space = rest.trim_start_matches(space);
    let standalone = if after_space.starts_with("standalone") {
        if after_space.len() == rest.len() {
            return Err("Space required.");
        }
        rest = after_space;
        Some(
            match attribute(
                &mut rest,
                "standalone",
                "standalone accepts only 'yes' or 'no'.",
            )? {
                "yes" => true,
                "no" => false,
                _ => return Err("standalone accepts only 'yes' or 'no'."),
            },
        )
    } else {
        None
    };
    rest = rest
        .trim_start_matches(space)
        .strip_prefix("?>")
        .ok_or("Parsing XML declaration: '?>' expected.")?;
    let length = input.len() - rest.len();
    if !input[..length].is_ascii() {
        return Err("Invalid character value.");
    }
    Ok(Declaration {
        length,
        version: Some(version),
        standalone,
    })
}

pub(super) fn has_doctype(mut input: &str) -> bool {
    loop {
        input = input.trim_start_matches(space);
        if input.starts_with("<!DOCTYPE") {
            return true;
        }
        if let Some(comment) = input.strip_prefix("<!--") {
            let Some(end) = comment.find("--") else {
                return false;
            };
            let Some(rest) = comment[end..].strip_prefix("-->") else {
                return false;
            };
            input = rest;
        } else if let Some(instruction) = input.strip_prefix("<?") {
            let Some(end) = instruction.find("?>") else {
                return false;
            };
            input = &instruction[end + 2..];
        } else {
            return false;
        }
    }
}
