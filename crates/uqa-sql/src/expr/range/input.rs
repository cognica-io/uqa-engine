//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed range syntax and ordered multirange members, before subtype input.

use uqa_core::memory::{Produced, ProductionControl, ProductionString};

use crate::{error::Result, SQLError};

pub(super) struct BoundInput<'a>(&'a str);

impl BoundInput<'_> {
    pub(super) fn decode(&self, control: &ProductionControl<'_>) -> Result<Produced<String>> {
        let mut output = ProductionString::new(*control);
        let mut characters = self.0.chars().peekable();
        let mut quoted = false;
        while let Some(character) = characters.next() {
            control.check()?;
            match character {
                '\\' => output.push(characters.next().expect("validated escape"))?,
                '"' if quoted && characters.peek() == Some(&'"') => {
                    characters.next();
                    output.push('"')?;
                }
                '"' => quoted = !quoted,
                character => output.push(character)?,
            }
        }
        debug_assert!(!quoted);
        Ok(output.finish()?)
    }
}

pub(super) struct RangeInput<'a> {
    pub lower: Option<BoundInput<'a>>,
    pub upper: Option<BoundInput<'a>>,
    pub lower_inclusive: bool,
    pub upper_inclusive: bool,
}

struct Cursor<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<char> {
        self.text[self.offset..].chars().next()
    }

    fn take(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        Some(character)
    }

    fn whitespace(&mut self, control: &ProductionControl<'_>) -> Result<()> {
        while self.peek().is_some_and(syntax_whitespace) {
            control.check()?;
            self.take();
        }
        Ok(())
    }

    fn empty_keyword(&self) -> bool {
        self.text[self.offset..]
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("empty"))
    }

    fn bound(&mut self, control: &ProductionControl<'_>) -> Result<Option<BoundInput<'a>>> {
        if matches!(self.peek(), Some(',' | ')' | ']')) {
            return Ok(None);
        }
        let start = self.offset;
        let mut quoted = false;
        loop {
            control.check()?;
            let character = self
                .peek()
                .ok_or_else(|| malformed(self.text, false, "Unexpected end of input."))?;
            if !quoted && matches!(character, ',' | ')' | ']') {
                return Ok(Some(BoundInput(&self.text[start..self.offset])));
            }
            self.take();
            match character {
                '\\' => {
                    self.take()
                        .ok_or_else(|| malformed(self.text, false, "Unexpected end of input."))?;
                }
                '"' if quoted && self.peek() == Some('"') => {
                    self.take();
                }
                '"' => quoted = !quoted,
                _ => {}
            }
        }
    }
}

pub(super) fn syntax_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}')
}

fn malformed(text: &str, multirange: bool, detail: &str) -> SQLError {
    let kind = if multirange { "multirange" } else { "range" };
    SQLError::Diagnostic {
        sqlstate: "22P02".into(),
        message: format!("malformed {kind} literal: \"{text}\""),
        detail: Some(detail.into()),
        hint: None,
    }
}

/// Validate the complete range shape before invoking either subtype input function. Bound text is borrowed, so validating the upper bound does not retain an unnecessary lower-bound copy.
pub(super) fn range<'a>(
    text: &'a str,
    control: &ProductionControl<'_>,
) -> Result<Option<RangeInput<'a>>> {
    control.check()?;
    let mut cursor = Cursor { text, offset: 0 };
    cursor.whitespace(control)?;
    if cursor.empty_keyword() {
        cursor.offset += 5;
        cursor.whitespace(control)?;
        if cursor.peek().is_some() {
            return Err(malformed(text, false, "Junk after \"empty\" key word."));
        }
        return Ok(None);
    }
    let lower_inclusive = match cursor.take() {
        Some('[') => true,
        Some('(') => false,
        _ => {
            return Err(malformed(
                text,
                false,
                "Missing left parenthesis or bracket.",
            ))
        }
    };
    let lower = cursor.bound(control)?;
    if cursor.take() != Some(',') {
        return Err(malformed(text, false, "Missing comma after lower bound."));
    }
    let upper = cursor.bound(control)?;
    let upper_inclusive = match cursor.take() {
        Some(']') => true,
        Some(')') => false,
        _ => return Err(malformed(text, false, "Too many commas.")),
    };
    cursor.whitespace(control)?;
    if cursor.peek().is_some() {
        return Err(malformed(
            text,
            false,
            "Junk after right parenthesis or bracket.",
        ));
    }
    Ok(Some(RangeInput {
        lower,
        upper,
        lower_inclusive,
        upper_inclusive,
    }))
}

#[derive(Clone, Copy)]
enum Position {
    First,
    Next,
    After,
    Finished,
}

#[derive(Clone, Copy)]
enum Quoting {
    Ordinary,
    Escaped,
    Quoted,
    QuotedEscaped,
}

pub(super) struct MultirangeInput<'a> {
    cursor: Cursor<'a>,
    position: Position,
}

impl<'a> MultirangeInput<'a> {
    pub(super) fn new(text: &'a str, control: &ProductionControl<'_>) -> Result<Self> {
        control.check()?;
        let mut cursor = Cursor { text, offset: 0 };
        cursor.whitespace(control)?;
        if cursor.take() != Some('{') {
            return Err(malformed(text, true, "Missing left brace."));
        }
        Ok(Self {
            cursor,
            position: Position::First,
        })
    }

    fn finish(&mut self, control: &ProductionControl<'_>) -> Result<Option<&'a str>> {
        self.cursor.take();
        self.cursor.whitespace(control)?;
        if self.cursor.peek().is_some() {
            return Err(malformed(
                self.cursor.text,
                true,
                "Junk after closing right brace.",
            ));
        }
        self.position = Position::Finished;
        Ok(None)
    }

    /// Return one member before inspecting the next separator, preserving subtype-error precedence over later malformed members.
    pub(super) fn next_range(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&'a str>> {
        loop {
            control.check()?;
            if matches!(self.position, Position::Finished) {
                return Ok(None);
            }
            self.cursor.whitespace(control)?;
            let character = self
                .cursor
                .peek()
                .ok_or_else(|| malformed(self.cursor.text, true, "Unexpected end of input."))?;
            if matches!(self.position, Position::After) {
                match character {
                    '}' => return self.finish(control),
                    ',' => {
                        self.cursor.take();
                        self.position = Position::Next;
                        continue;
                    }
                    _ => {
                        return Err(malformed(
                            self.cursor.text,
                            true,
                            "Expected comma or end of multirange.",
                        ))
                    }
                }
            }
            if character == '}' && matches!(self.position, Position::First) {
                return self.finish(control);
            }
            let start = self.cursor.offset;
            if self.cursor.empty_keyword() {
                self.cursor.offset += 5;
            } else if matches!(character, '[' | '(') {
                self.member_end(control)?;
            } else {
                return Err(malformed(self.cursor.text, true, "Expected range start."));
            }
            self.position = Position::After;
            return Ok(Some(&self.cursor.text[start..self.cursor.offset]));
        }
    }

    fn member_end(&mut self, control: &ProductionControl<'_>) -> Result<()> {
        self.cursor.take();
        let mut quoting = Quoting::Ordinary;
        loop {
            control.check()?;
            let character = self
                .cursor
                .take()
                .ok_or_else(|| malformed(self.cursor.text, true, "Unexpected end of input."))?;
            if syntax_whitespace(character) {
                continue;
            }
            quoting = match (quoting, character) {
                (Quoting::Ordinary, ')' | ']') => return Ok(()),
                (Quoting::Ordinary, '"') => Quoting::Quoted,
                (Quoting::Ordinary, '\\') => Quoting::Escaped,
                (Quoting::Escaped, _) => Quoting::Ordinary,
                (Quoting::Quoted, '"') if self.cursor.peek() != Some('"') => Quoting::Ordinary,
                (Quoting::Quoted, '"') => {
                    self.cursor.take();
                    Quoting::Quoted
                }
                (Quoting::Quoted, '\\') => Quoting::QuotedEscaped,
                (Quoting::QuotedEscaped, _) => Quoting::Quoted,
                (state, _) => state,
            };
        }
    }
}
