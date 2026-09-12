//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared replacement fragments retain the regex capture-expansion contract without per-match strings.

use std::borrow::Cow;
use std::ops::Range;

use regex::{CaptureLocations, Regex};

#[derive(Debug)]
enum Piece {
    Literal(Range<usize>),
    Capture(usize),
}

#[derive(Debug)]
pub(crate) struct Replacement<'a> {
    text: Cow<'a, str>,
    pieces: Option<Vec<Piece>>,
    uses_captures: bool,
}

impl<'a> Replacement<'a> {
    pub fn literal(text: &'a str) -> Self {
        Self {
            text: Cow::Borrowed(text),
            pieces: None,
            uses_captures: false,
        }
    }

    pub fn prepare(text: &'a str, expression: &Regex) -> Self {
        if !text.contains('$') {
            return Self::literal(text);
        }
        let mut pieces = Vec::new();
        let mut cursor = 0;
        let mut uses_captures = false;
        while let Some(relative) = text[cursor..].find('$') {
            let dollar = cursor + relative;
            if cursor < dollar {
                pieces.push(Piece::Literal(cursor..dollar));
            }
            cursor = dollar + 1;
            if text.as_bytes().get(cursor) == Some(&b'$') {
                pieces.push(Piece::Literal(dollar..cursor));
                cursor += 1;
                continue;
            }
            let reference = if text.as_bytes().get(cursor) == Some(&b'{') {
                text[cursor + 1..].find('}').map(|length| {
                    let end = cursor + 1 + length;
                    (cursor + 1..end, end + 1)
                })
            } else {
                let start = cursor;
                while text
                    .as_bytes()
                    .get(cursor)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    cursor += 1;
                }
                (cursor != start).then_some((start..cursor, cursor))
            };
            let Some((name, end)) = reference else {
                pieces.push(Piece::Literal(dollar..dollar + 1));
                cursor = dollar + 1;
                continue;
            };
            cursor = end;
            let name = &text[name];
            let index = name.parse::<usize>().ok().or_else(|| {
                expression
                    .capture_names()
                    .position(|candidate| candidate == Some(name))
            });
            if let Some(index) = index.filter(|index| *index < expression.captures_len()) {
                uses_captures = true;
                pieces.push(Piece::Capture(index));
            }
        }
        if cursor < text.len() {
            pieces.push(Piece::Literal(cursor..text.len()));
        }
        Self {
            text: Cow::Borrowed(text),
            pieces: Some(pieces),
            uses_captures,
        }
    }

    pub fn into_owned(self) -> Replacement<'static> {
        Replacement {
            text: Cow::Owned(self.text.into_owned()),
            pieces: self.pieces,
            uses_captures: self.uses_captures,
        }
    }

    pub fn uses_captures(&self) -> bool {
        self.uses_captures
    }

    pub fn fragments<'b>(
        &'b self,
        locations: Option<&'b CaptureLocations>,
        input: &'b str,
    ) -> impl Iterator<Item = &'b str> + Clone {
        std::iter::once(self.pieces.is_none().then_some(self.text.as_ref()))
            .flatten()
            .chain(
                self.pieces
                    .iter()
                    .flat_map(|pieces| pieces.iter())
                    .filter_map(move |piece| match piece {
                        Piece::Literal(range) => Some(&self.text[range.clone()]),
                        Piece::Capture(index) => locations
                            .and_then(|locations| locations.get(*index))
                            .map(|(start, end)| &input[start..end]),
                    }),
            )
    }
}
