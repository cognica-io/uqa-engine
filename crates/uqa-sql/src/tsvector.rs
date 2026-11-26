//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text input and output for the `PostgreSQL` full-text vector type.

use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

/// One normalized lexeme and its sorted document positions. Weight zero is D and weight three is A.
pub struct Lexeme {
    pub text: String,
    pub positions: Vec<(u16, u8)>,
}

fn error(state: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: state.into(),
        message,
    }
}

struct Input<'a> {
    source: &'a str,
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}
impl Input<'_> {
    fn syntax(&self) -> SQLError {
        error(
            "42601",
            format!("syntax error in tsvector: \"{}\"", self.source),
        )
    }

    fn word(
        &mut self,
        first: char,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        let mut word = ProductionString::new(*control);
        let quoted = first == '\'';
        let mut next = if quoted {
            self.chars.next()
        } else {
            Some(first)
        };
        while let Some(c) = next {
            if c == '\\' {
                let c = self.chars.next().ok_or_else(|| {
                    error(
                        "42601",
                        format!("there is no escaped character: \"{}\"", self.source),
                    )
                })?;
                word.push(c)?;
            } else if quoted && c == '\'' {
                if self.chars.next_if_eq(&'\'').is_none() {
                    if word.is_empty() {
                        return Err(self.syntax());
                    }
                    return Ok(word.finish()?);
                }
                word.push(c)?;
            } else {
                word.push(c)?;
            }
            if !quoted && self.chars.peek().is_none_or(|c| space(*c) || *c == ':') {
                return Ok(word.finish()?);
            }
            next = self.chars.next();
        }
        Err(self.syntax())
    }

    fn position(&mut self, control: &ProductionControl<'_>) -> Result<(u16, u8), SQLError> {
        if !self.chars.peek().is_some_and(char::is_ascii_digit) {
            return Err(self.syntax());
        }
        let mut position = 0_u32;
        while let Some(c) = self.chars.next_if(char::is_ascii_digit) {
            control.check()?;
            position = position
                .saturating_mul(10)
                .saturating_add(c as u32 - '0' as u32);
        }
        if position == 0 {
            return Err(error(
                "42601",
                format!("wrong position info in tsvector: \"{}\"", self.source),
            ));
        }
        let mut weight = 0;
        while let Some(c) = self.chars.next_if(|c| !space(*c) && *c != ',') {
            control.check()?;
            if c.is_ascii_digit() {
                continue;
            }
            if weight != 0 {
                return Err(self.syntax());
            }
            weight = match c.to_ascii_uppercase() {
                'A' | '*' => 3,
                'B' => 2,
                'C' => 1,
                'D' => 0,
                _ => return Err(self.syntax()),
            };
        }
        Ok((position.min(16383) as u16, weight))
    }
}

fn space(c: char) -> bool {
    c.is_ascii_whitespace() || c == '\u{b}'
}
const MAX_STRING_OFFSET: usize = (1 << 20) - 1;

fn check_total(bytes: usize) -> Result<(), SQLError> {
    if bytes > MAX_STRING_OFFSET {
        Err(error(
            "54000",
            format!(
                "string is too long for tsvector ({bytes} bytes, max {MAX_STRING_OFFSET} bytes)"
            ),
        ))
    } else {
        Ok(())
    }
}

/// Read unquoted and quoted lexemes, combine repeated words, then coalesce equal positions with their strongest weight.
pub fn parse(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Lexeme>>, SQLError> {
    let mut input = Input {
        source: text,
        chars: text.chars().peekable(),
    };
    let mut entries = ProductionVec::new(*control);
    let mut raw_bytes = 0;
    while let Some(first) = input.chars.next() {
        control.check()?;
        if space(first) {
            continue;
        }
        let word = input.word(first, control)?;
        let mut positions = ProductionVec::new(*control);
        if input.chars.next_if_eq(&':').is_some() {
            loop {
                positions.push_copy(input.position(control)?)?;
                if input.chars.next_if_eq(&',').is_none() {
                    break;
                }
            }
        }
        if word.len() > 2046 {
            return Err(error(
                "54000",
                format!("word is too long ({} bytes, max 2046 bytes)", word.len()),
            ));
        }
        check_total(raw_bytes)?;
        raw_bytes += word.len();
        let (text, text_memory) = word.into_parts();
        let (positions, position_memory) = positions.finish()?.into_parts();
        entries.push_produced(control.finish(
            Lexeme { text, positions },
            control.combine(text_memory, position_memory),
        )?)?;
    }
    let (mut entries, _entry_memory) = entries.finish()?.into_parts();
    entries.sort_unstable_by(|a, b| a.text.cmp(&b.text));
    let mut output = ProductionVec::new(*control);
    let mut bytes = 0usize;
    for words in entries.chunk_by(|a, b| a.text == b.text) {
        control.check()?;
        let mut ordered = ProductionVec::new(*control);
        for word in words {
            for &position in &word.positions {
                ordered.push_copy(position)?;
            }
        }
        ordered
            .as_mut_slice()
            .sort_unstable_by_key(|&(position, weight)| (position, std::cmp::Reverse(weight)));
        let mut positions = ProductionVec::new(*control);
        for same in ordered.chunk_by(|a, b| a.0 == b.0).take(256) {
            positions.push_copy(same[0])?;
        }
        let (text, text_memory) = control.copy_text(&words[0].text)?.into_parts();
        let (positions, position_memory) = positions.finish()?.into_parts();
        bytes += text.len();
        if !positions.is_empty() {
            bytes = bytes.next_multiple_of(2) + 2 + 2 * positions.len();
        }
        output.push_produced(control.finish(
            Lexeme { text, positions },
            control.combine(text_memory, position_memory),
        )?)?;
    }
    check_total(bytes)?;
    Ok(output.finish()?)
}

pub(crate) fn canonical_text(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<uqa_core::Value>, SQLError> {
    let entries = parse(input, control)?;
    let mut text = ProductionString::new(*control);
    for entry in entries.iter() {
        if !text.is_empty() {
            text.push(' ')?;
        }
        text.push('\'')?;
        for c in entry.text.chars() {
            if matches!(c, '\'' | '\\') {
                text.push(c)?;
            }
            text.push(c)?;
        }
        text.push('\'')?;
        for (i, &(pos, weight)) in entry.positions.iter().enumerate() {
            text.push(if i == 0 { ':' } else { ',' })?;
            text.push_str(&control.format(format_args!("{pos}"))?)?;
            if weight > 0 {
                text.push(char::from(b'D' - weight))?;
            }
        }
    }
    let (text, memory) = text.finish()?.into_parts();
    Ok(control.finish(uqa_core::Value::Str(text), memory)?)
}
