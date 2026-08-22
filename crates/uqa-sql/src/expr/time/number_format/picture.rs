//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parsing for `PostgreSQL` numeric format pictures.

use crate::error::{Result, SQLError};

pub(super) const NUMERIC_MINUS_MARKER: char = '\u{1}';
pub(super) const NUMERIC_PLUS_MARKER: char = '\u{2}';
pub(super) const NUMERIC_SIGN_MARKER: char = '\u{3}';
pub(super) const NUMERIC_BRACKET_MARKER: char = '\u{4}';

pub(super) struct NumericPicture {
    pub(super) fill_mode: bool,
    pub(super) template: String,
    pub(super) sign_format: NumericSignFormat,
}

pub(super) fn parse_numeric_picture(fmt: &str) -> Result<Option<NumericPicture>> {
    let mut fill_mode = false;
    let mut template = String::with_capacity(fmt.len());
    let mut explicit_count = 0usize;
    let mut explicit_plus = false;
    let mut explicit_minus = false;
    let mut last_explicit = None;
    let mut locale_sign_position = None;
    let mut brackets = false;
    let mut remaining = fmt;
    while !remaining.is_empty() {
        if let Some(rest) = remaining
            .strip_prefix("FM")
            .or_else(|| remaining.strip_prefix("fm"))
        {
            fill_mode = true;
            remaining = rest;
            continue;
        }
        let mut matched_explicit = false;
        for (upper, lower, explicit) in [
            ("MI", "mi", ExplicitNumericSign::Minus),
            ("PL", "pl", ExplicitNumericSign::Plus),
            ("SG", "sg", ExplicitNumericSign::Sign),
        ] {
            if let Some(rest) = remaining
                .strip_prefix(upper)
                .or_else(|| remaining.strip_prefix(lower))
            {
                if locale_sign_position.is_some() {
                    return Err(numeric_format_syntax(format!(
                        "cannot use \"S\" and \"{upper}\" together"
                    )));
                }
                if brackets {
                    return Err(numeric_format_syntax(
                        "cannot use \"PR\" and \"S\"/\"PL\"/\"MI\"/\"SG\" together",
                    ));
                }
                template.push(explicit.marker());
                explicit_count += 1;
                explicit_plus |= matches!(
                    explicit,
                    ExplicitNumericSign::Plus | ExplicitNumericSign::Sign
                );
                explicit_minus |= matches!(
                    explicit,
                    ExplicitNumericSign::Minus | ExplicitNumericSign::Sign
                );
                last_explicit = Some(explicit);
                remaining = rest;
                matched_explicit = true;
                break;
            }
        }
        if matched_explicit {
            continue;
        }
        if let Some(rest) = remaining
            .strip_prefix("PR")
            .or_else(|| remaining.strip_prefix("pr"))
        {
            if locale_sign_position.is_some() || explicit_count != 0 {
                return Err(numeric_format_syntax(
                    "cannot use \"PR\" and \"S\"/\"PL\"/\"MI\"/\"SG\" together",
                ));
            }
            brackets = true;
            remaining = rest;
            continue;
        }
        if let Some(rest) = remaining
            .strip_prefix('S')
            .or_else(|| remaining.strip_prefix('s'))
        {
            if locale_sign_position.is_some() {
                return Err(numeric_format_syntax("cannot use \"S\" twice"));
            }
            if explicit_count != 0 || brackets {
                return Err(numeric_format_syntax(
                    "cannot use \"S\" and \"PL\"/\"MI\"/\"SG\"/\"PR\" together",
                ));
            }
            locale_sign_position = Some(template.len());
            remaining = rest;
            continue;
        }
        let Some(character) = remaining.chars().next() else {
            return Ok(None);
        };
        if !matches!(character, '9' | '0' | '.' | ',') {
            return Ok(None);
        }
        if brackets && matches!(character, '9' | '0') {
            return Err(numeric_format_syntax(format!(
                "\"{character}\" must be ahead of \"PR\""
            )));
        }
        if character == '.' && template.contains('.') {
            return Err(numeric_format_syntax("multiple decimal points"));
        }
        template.push(character);
        remaining = &remaining[character.len_utf8()..];
    }
    if !template
        .chars()
        .any(|character| matches!(character, '9' | '0'))
        || template.matches('.').count() > 1
    {
        return Ok(None);
    }
    let sign_format = if brackets {
        insert_after_last_numeric_token(&mut template, NUMERIC_BRACKET_MARKER);
        NumericSignFormat::Brackets
    } else if let Some(position) = locale_sign_position {
        let pre_end = template.find('.').unwrap_or(template.len());
        if position < pre_end
            && template[position..pre_end]
                .chars()
                .any(|token| matches!(token, '9' | '0'))
        {
            NumericSignFormat::Leading
        } else {
            insert_after_last_numeric_token(&mut template, NUMERIC_SIGN_MARKER);
            NumericSignFormat::Trailing
        }
    } else if explicit_count == 1
        && last_explicit.is_some_and(|explicit| template.ends_with(explicit.marker()))
    {
        let explicit = last_explicit.expect("one explicit numeric sign exists");
        template.pop();
        explicit.trailing_format()
    } else if explicit_count != 0 {
        NumericSignFormat::Explicit {
            implicit_sign: explicit_plus && !explicit_minus,
        }
    } else {
        NumericSignFormat::Default
    };
    Ok(Some(NumericPicture {
        fill_mode,
        template,
        sign_format,
    }))
}

fn insert_after_last_numeric_token(template: &mut String, marker: char) {
    let position = template
        .char_indices()
        .rfind(|(_, token)| matches!(token, '9' | '0' | '.'))
        .map_or(template.len(), |(position, token)| {
            position + token.len_utf8()
        });
    template.insert(position, marker);
}

fn numeric_format_syntax(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: message.into(),
    }
}

#[derive(Clone, Copy)]
enum ExplicitNumericSign {
    Minus,
    Plus,
    Sign,
}

impl ExplicitNumericSign {
    fn marker(self) -> char {
        match self {
            Self::Minus => NUMERIC_MINUS_MARKER,
            Self::Plus => NUMERIC_PLUS_MARKER,
            Self::Sign => NUMERIC_SIGN_MARKER,
        }
    }

    fn trailing_format(self) -> NumericSignFormat {
        match self {
            Self::Minus => NumericSignFormat::Minus,
            Self::Plus => NumericSignFormat::Plus,
            Self::Sign => NumericSignFormat::Sign,
        }
    }
}

pub(super) fn split_outer_numeric_literals(fmt: &str) -> Option<(&str, &str, &str)> {
    let mut quoted = false;
    let mut escaped = false;
    let mut first = None;
    let mut end = 0usize;
    for (position, character) in fmt.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted && matches!(character, '9' | '0' | '.' | ',') {
            first.get_or_insert(position);
            end = position + character.len_utf8();
        }
    }
    let mut start = first?;
    loop {
        let prefix = &fmt[..start];
        if prefix.ends_with("FM") || prefix.ends_with("fm") {
            start -= 2;
        } else if prefix.ends_with('S') || prefix.ends_with('s') {
            start -= 1;
        } else {
            break;
        }
    }
    loop {
        let mut matched = false;
        for token in [
            "FM", "fm", "MI", "mi", "PL", "pl", "SG", "sg", "PR", "pr", "S", "s",
        ] {
            if fmt[end..].starts_with(token) {
                end += token.len();
                matched = true;
                break;
            }
        }
        if !matched {
            break;
        }
    }
    Some((&fmt[..start], &fmt[start..end], &fmt[end..]))
}

pub(super) fn decode_numeric_format_literal(fragment: &str) -> String {
    let mut output = String::with_capacity(fragment.len());
    let mut characters = fragment.chars().peekable();
    let mut quoted = false;
    while let Some(character) = characters.next() {
        if character == '"' {
            quoted = !quoted;
            continue;
        }
        if character == '\\' {
            if quoted {
                if let Some(character) = characters.next() {
                    output.push(character);
                }
                continue;
            }
            if characters.peek() == Some(&'"') {
                output.push(characters.next().expect("peeked quote must exist"));
                continue;
            }
        }
        output.push(character);
    }
    output
}

#[derive(Clone, Copy)]
pub(super) enum NumericSignFormat {
    Default,
    Leading,
    Trailing,
    Explicit { implicit_sign: bool },
    Minus,
    Plus,
    Sign,
    Brackets,
}

pub(super) fn is_explicit_numeric_sign_marker(character: char) -> bool {
    matches!(
        character,
        NUMERIC_MINUS_MARKER | NUMERIC_PLUS_MARKER | NUMERIC_SIGN_MARKER
    )
}

pub(super) fn is_numeric_picture_marker(character: char) -> bool {
    is_explicit_numeric_sign_marker(character) || character == NUMERIC_BRACKET_MARKER
}
