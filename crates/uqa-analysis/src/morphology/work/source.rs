//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained source scans preserve original Java line boundaries and UTF-16 units.

use super::Work;
use crate::morphology::{io::vector, DictionaryResult};

pub(crate) struct Lines<'a> {
    source: &'a str,
    position: Option<usize>,
}
impl<'a> Lines<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            position: Some(0),
        }
    }
    pub async fn next(&mut self, work: &mut impl Work) -> DictionaryResult<Option<&'a str>> {
        let Some(start) = self.position else {
            return Ok(None);
        };
        for (index, bytes) in self.source.as_bytes()[start..].chunks(4096).enumerate() {
            work.checkpoint().await?;
            if let Some(offset) = bytes.iter().position(|byte| matches!(byte, b'\r' | b'\n')) {
                let end = start + index * 4096 + offset;
                self.position = Some(end + 1);
                return Ok(Some(&self.source[start..end]));
            }
        }
        self.position = None;
        Ok(Some(&self.source[start..]))
    }
}

pub(crate) async fn blank(value: &str, work: &mut impl Work) -> DictionaryResult<bool> {
    for (index, ch) in value.chars().enumerate() {
        work.records(index).await?;
        if ch > '\u{20}' {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) async fn utf16_len(value: &str, work: &mut impl Work) -> DictionaryResult<usize> {
    let (mut position, mut count) = (0, 0);
    while position < value.len() {
        work.checkpoint().await?;
        let part = crate::json::fragment(value, position);
        count += part.encode_utf16().count();
        position += part.len();
    }
    Ok(count)
}

pub(crate) async fn utf16(
    value: &str,
    length: usize,
    work: &mut impl Work,
) -> DictionaryResult<Vec<u16>> {
    let mut output = vector(length)?;
    let mut position = 0;
    while position < value.len() {
        work.checkpoint().await?;
        let part = crate::json::fragment(value, position);
        output.extend(part.encode_utf16());
        position += part.len();
    }
    Ok(output)
}

pub(crate) async fn text(value: &str, work: &mut impl Work) -> DictionaryResult<String> {
    let mut output = String::new();
    output.try_reserve_exact(value.len())?;
    append(&mut output, value, work).await?;
    Ok(output)
}

pub(crate) async fn append(
    output: &mut String,
    value: &str,
    work: &mut impl Work,
) -> DictionaryResult<()> {
    let mut position = 0;
    while position < value.len() {
        work.checkpoint().await?;
        let part = crate::json::fragment(value, position);
        output.push_str(part);
        position += part.len();
    }
    Ok(())
}
