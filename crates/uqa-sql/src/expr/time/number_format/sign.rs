//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sign placement for `PostgreSQL` numeric format pictures.

use super::{
    default_numeric_sign, is_explicit_numeric_sign_marker, NUMERIC_BRACKET_MARKER,
    NUMERIC_MINUS_MARKER, NUMERIC_PLUS_MARKER, NUMERIC_SIGN_MARKER,
};

pub(super) fn apply_positioned_numeric_sign(mut body: String, negative: bool) -> String {
    let sign = if negative { "-" } else { "+" };
    if let Some(position) = body.find(NUMERIC_SIGN_MARKER) {
        body.replace_range(position..position + NUMERIC_SIGN_MARKER.len_utf8(), sign);
    } else {
        body.push_str(sign);
    }
    body
}

pub(super) fn apply_numeric_brackets(mut body: String, negative: bool, fill_mode: bool) -> String {
    if let Some(boundary) = body.find(NUMERIC_BRACKET_MARKER) {
        body.replace_range(
            boundary..boundary + NUMERIC_BRACKET_MARKER.len_utf8(),
            if negative {
                ">"
            } else if fill_mode {
                ""
            } else {
                " "
            },
        );
    } else if negative {
        body.push('>');
    } else if !fill_mode {
        body.push(' ');
    }
    if negative {
        let first_digit = body.find(|character: char| character != ' ').unwrap_or(0);
        body.insert(first_digit, '<');
        body
    } else if fill_mode {
        body
    } else {
        default_numeric_sign(body, false, false)
    }
}

pub(super) fn apply_explicit_numeric_signs(
    body: String,
    negative: bool,
    fill_mode: bool,
    implicit_sign: bool,
) -> String {
    let implicit_sign_position = implicit_sign.then(|| {
        body.char_indices()
            .find(|(_, character)| {
                *character != ' ' && !is_explicit_numeric_sign_marker(*character)
            })
            .map_or(body.len(), |(position, _)| position)
    });
    let mut output =
        String::with_capacity(body.len() + usize::from(implicit_sign_position.is_some()));
    for (position, character) in body.char_indices() {
        if implicit_sign_position == Some(position) && (!fill_mode || negative) {
            output.push(if negative { '-' } else { ' ' });
        }
        match character {
            NUMERIC_MINUS_MARKER if negative => output.push('-'),
            NUMERIC_MINUS_MARKER if !fill_mode => output.push(' '),
            NUMERIC_MINUS_MARKER => {}
            NUMERIC_PLUS_MARKER if !negative => output.push('+'),
            NUMERIC_PLUS_MARKER if !fill_mode => output.push(' '),
            NUMERIC_PLUS_MARKER => {}
            NUMERIC_SIGN_MARKER => output.push(if negative { '-' } else { '+' }),
            character => output.push(character),
        }
    }
    if implicit_sign_position == Some(body.len()) && (!fill_mode || negative) {
        output.push(if negative { '-' } else { ' ' });
    }
    output
}
