//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sign placement for `PostgreSQL` numeric format pictures.

use super::picture::{
    is_explicit_numeric_sign_marker, NumericSignFormat, NUMERIC_BRACKET_MARKER,
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

pub(super) fn apply_truncated_special_sign(
    body: String,
    negative: bool,
    fill_mode: bool,
    sign_format: NumericSignFormat,
) -> String {
    match sign_format {
        NumericSignFormat::Default => default_numeric_sign(body, negative, fill_mode),
        NumericSignFormat::Leading => anchored_numeric_sign(body, negative),
        NumericSignFormat::Explicit { implicit_sign } => {
            apply_explicit_numeric_signs(body, negative, fill_mode, implicit_sign)
        }
        NumericSignFormat::Trailing | NumericSignFormat::Minus | NumericSignFormat::Sign => body,
        NumericSignFormat::Plus if fill_mode => {
            if negative {
                format!("-{body}")
            } else {
                body
            }
        }
        NumericSignFormat::Plus => default_numeric_sign(body, negative, false),
        NumericSignFormat::Brackets if negative => {
            let first_digit = body.find(|character: char| character != ' ').unwrap_or(0);
            let (padding, number) = body.split_at(first_digit);
            format!("{padding}<{number}")
        }
        NumericSignFormat::Brackets => default_numeric_sign(body, false, fill_mode),
    }
}

pub(super) fn apply_float_aware_numeric_sign(
    body: String,
    negative: bool,
    fill_mode: bool,
    sign_format: NumericSignFormat,
    truncation: FloatFractionTruncation,
) -> String {
    if truncation == FloatFractionTruncation::None {
        return apply_numeric_sign(body, negative, fill_mode, sign_format);
    }
    let truncated_fraction_emits_sign = truncation == FloatFractionTruncation::NinePlaceholder;
    match sign_format {
        NumericSignFormat::Default => default_numeric_sign(body, negative, fill_mode),
        NumericSignFormat::Leading => anchored_numeric_sign(body, negative),
        NumericSignFormat::Explicit { implicit_sign } => {
            apply_explicit_numeric_signs(body, negative, fill_mode, implicit_sign)
        }
        NumericSignFormat::Trailing => apply_positioned_numeric_sign(body, negative),
        NumericSignFormat::Minus if fill_mode && negative && truncated_fraction_emits_sign => {
            format!("{body}-")
        }
        NumericSignFormat::Minus => body,
        NumericSignFormat::Sign if fill_mode && truncated_fraction_emits_sign => {
            format!("{body}{}", if negative { '-' } else { '+' })
        }
        NumericSignFormat::Sign => body,
        NumericSignFormat::Plus if fill_mode => {
            if negative {
                format!("-{body}")
            } else if truncated_fraction_emits_sign {
                format!("{body}+")
            } else {
                body
            }
        }
        NumericSignFormat::Plus => default_numeric_sign(body, negative, false),
        NumericSignFormat::Brackets => {
            apply_numeric_sign(body, negative, fill_mode, NumericSignFormat::Brackets)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FloatFractionTruncation {
    None,
    ZeroPlaceholder,
    NinePlaceholder,
}

impl FloatFractionTruncation {
    pub(super) fn stops_picture_scan(self, fill_mode: bool) -> bool {
        self != Self::None && (!fill_mode || self == Self::ZeroPlaceholder)
    }
}

pub(super) fn apply_numeric_sign(
    body: String,
    negative: bool,
    fill_mode: bool,
    sign_format: NumericSignFormat,
) -> String {
    match sign_format {
        NumericSignFormat::Default => default_numeric_sign(body, negative, fill_mode),
        NumericSignFormat::Leading => anchored_numeric_sign(body, negative),
        NumericSignFormat::Explicit { implicit_sign } => {
            apply_explicit_numeric_signs(body, negative, fill_mode, implicit_sign)
        }
        NumericSignFormat::Trailing => apply_positioned_numeric_sign(body, negative),
        NumericSignFormat::Sign => format!("{body}{}", if negative { '-' } else { '+' }),
        NumericSignFormat::Minus => {
            if negative {
                format!("{body}-")
            } else if fill_mode {
                body
            } else {
                format!("{body} ")
            }
        }
        NumericSignFormat::Plus => {
            if fill_mode {
                if negative {
                    format!("-{body}")
                } else {
                    format!("{body}+")
                }
            } else {
                format!(
                    "{}{}",
                    default_numeric_sign(body, negative, false),
                    if negative { ' ' } else { '+' }
                )
            }
        }
        NumericSignFormat::Brackets => apply_numeric_brackets(body, negative, fill_mode),
    }
}

fn default_numeric_sign(mut body: String, negative: bool, fill_mode: bool) -> String {
    if !negative {
        return if fill_mode { body } else { format!(" {body}") };
    }
    if !fill_mode {
        let leading_spaces = body.bytes().take_while(|byte| *byte == b' ').count();
        if leading_spaces > 0 {
            body.replace_range(leading_spaces - 1..leading_spaces, "-");
            return format!(" {body}");
        }
    }
    format!("-{body}")
}

fn anchored_numeric_sign(mut body: String, negative: bool) -> String {
    let sign = if negative { '-' } else { '+' };
    let leading_spaces = body.bytes().take_while(|byte| *byte == b' ').count();
    body.insert(leading_spaces, sign);
    body
}
