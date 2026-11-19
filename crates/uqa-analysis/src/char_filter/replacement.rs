//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared replacement fragments retain the regex capture-expansion contract without per-match strings.

use std::borrow::Cow;
use std::ops::Range;

use regex::Regex;

use crate::cooperative_regex::CaptureSlots;

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

    #[cfg(test)]
    pub fn prepare(text: &'a str, expression: &Regex) -> Self {
        crate::work::inline(Self::prepare_with(text, expression, &mut ()))
            .unwrap_or_else(|never: std::convert::Infallible| match never {})
    }

    pub async fn prepare_with<E>(
        text: &'a str,
        expression: &Regex,
        work: &mut impl crate::work::Work<E>,
    ) -> Result<Self, E> {
        let Some(mut dollar) = crate::work::find_byte(text, 0, b'$', work).await? else {
            return Ok(Self::literal(text));
        };
        let mut pieces = Vec::new();
        let mut cursor = 0;
        let mut uses_captures = false;
        loop {
            if cursor < dollar {
                pieces.push(Piece::Literal(cursor..dollar));
            }
            cursor = dollar + 1;
            if text.as_bytes().get(cursor) == Some(&b'$') {
                pieces.push(Piece::Literal(dollar..cursor));
                cursor += 1;
            } else {
                let reference = if text.as_bytes().get(cursor) == Some(&b'{') {
                    crate::work::find_byte(text, cursor + 1, b'}', work)
                        .await?
                        .map(|end| (cursor + 1..end, end + 1))
                } else {
                    let start = cursor;
                    while text
                        .as_bytes()
                        .get(cursor)
                        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    {
                        work.records(cursor - start).await?;
                        cursor += 1;
                    }
                    (cursor != start).then_some((start..cursor, cursor))
                };
                if let Some((name, end)) = reference {
                    cursor = end;
                    let name = &text[name];
                    let index = match name.parse::<usize>().ok() {
                        Some(index) => Some(index),
                        None => capture_index(expression, name, work).await?,
                    };
                    if let Some(index) = index.filter(|index| *index < expression.captures_len()) {
                        uses_captures = true;
                        pieces.push(Piece::Capture(index));
                    }
                } else {
                    pieces.push(Piece::Literal(dollar..dollar + 1));
                    cursor = dollar + 1;
                }
            }
            let Some(next) = crate::work::find_byte(text, cursor, b'$', work).await? else {
                break;
            };
            dollar = next;
        }
        if cursor < text.len() {
            pieces.push(Piece::Literal(cursor..text.len()));
        }
        Ok(Self {
            text: Cow::Borrowed(text),
            pieces: Some(pieces),
            uses_captures,
        })
    }

    pub async fn into_owned_with<E>(
        self,
        work: &mut impl crate::work::Work<E>,
    ) -> Result<Replacement<'static>, E> {
        Ok(Replacement {
            text: Cow::Owned(crate::work::own_text(self.text, work).await?),
            pieces: self.pieces,
            uses_captures: self.uses_captures,
        })
    }

    pub fn uses_captures(&self) -> bool {
        self.uses_captures
    }

    pub fn fragments<'b>(
        &'b self,
        locations: Option<&'b CaptureSlots>,
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

async fn capture_index<E>(
    expression: &Regex,
    name: &str,
    work: &mut impl crate::work::Work<E>,
) -> Result<Option<usize>, E> {
    for (index, candidate) in expression.capture_names().enumerate() {
        work.records(index).await?;
        if let Some(candidate) = candidate {
            if crate::json::bytes_equal(candidate.as_bytes(), name.as_bytes(), work).await? {
                return Ok(Some(index));
            }
        }
    }
    Ok(None)
}
