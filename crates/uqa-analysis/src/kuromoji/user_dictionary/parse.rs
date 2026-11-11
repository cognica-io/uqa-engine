//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lucene CSV, Java line boundaries, and Japanese segmentation syntax.

use crate::kuromoji::error::{check_limit, invalid};
use crate::kuromoji::DictionaryResult;
use crate::morphology::{
    io::vector,
    work::{source, Work},
};

pub(super) struct Entry(pub Vec<String>);
impl crate::morphology::work::ordering::Surface for Entry {
    fn surface(&self) -> &str {
        &self.0[0]
    }
}

pub(super) async fn entries(
    text: &str,
    maximum: usize,
    work: &mut impl Work,
) -> DictionaryResult<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut lines = source::Lines::new(text);
    while let Some(line) = lines.next(work).await? {
        if source::blank(line, work).await? || comment(line, work).await? {
            continue;
        }
        check_limit("user dictionary entries", entries.len() + 1, maximum)?;
        let fields = csv(line, work).await?;
        if fields.len() < 4 {
            return Err(invalid(
                "user dictionary",
                "CSV entry has fewer than four fields",
            ));
        }
        entries.try_reserve(1)?;
        entries.push(Entry(fields));
    }
    Ok(entries)
}

async fn comment(line: &str, work: &mut impl Work) -> DictionaryResult<bool> {
    if !line.starts_with('#') {
        return Ok(false);
    }
    for (index, ch) in line.chars().enumerate() {
        work.records(index).await?;
        if matches!(ch, '\u{85}' | '\u{2028}' | '\u{2029}') {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn csv(line: &str, work: &mut impl Work) -> DictionaryResult<Vec<String>> {
    let mut fields = vector(4)?;
    let mut quoted = false;
    let mut start = 0;
    for (index, (offset, ch)) in line.char_indices().enumerate() {
        work.records(index).await?;
        if ch == '"' {
            quoted = !quoted;
        }
        if ch == ',' && !quoted {
            if fields.len() < 4 {
                fields.push(unquote(&line[start..offset], work).await?);
            }
            start = offset + 1;
        }
    }
    if quoted {
        return Err(invalid("user dictionary", "CSV quotes are unbalanced"));
    }
    // CSVUtil deliberately leaves its final field quoted and escaped.
    if fields.len() < 4 {
        fields.push(source::text(&line[start..], work).await?);
    }
    Ok(fields)
}

async fn quote(value: &str, start: usize, work: &mut impl Work) -> DictionaryResult<Option<usize>> {
    Ok(
        crate::work::find_byte::<crate::morphology::error::DictionaryError>(
            value, start, b'"', work,
        )
        .await?,
    )
}

async fn unquote(value: &str, work: &mut impl Work) -> DictionaryResult<String> {
    let value = match value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        Some(inner) if !inner.is_empty() && quote(inner, 0, work).await?.is_none() => inner,
        _ => value,
    };
    let mut output = String::new();
    output.try_reserve_exact(value.len())?;
    let (mut start, mut position) = (0, 0);
    while let Some(index) = quote(value, position, work).await? {
        if value.as_bytes().get(index + 1) == Some(&b'"') {
            source::append(&mut output, &value[start..index], work).await?;
            output.push('"');
            start = index + 2;
            position = start;
        } else {
            position = index + 1;
        }
    }
    source::append(&mut output, &value[start..], work).await?;
    Ok(output)
}

async fn next_non_whitespace(
    chars: &mut std::str::Chars<'_>,
    work: &mut impl Work,
) -> DictionaryResult<Option<char>> {
    for (index, ch) in chars.by_ref().enumerate() {
        work.records(index).await?;
        if !matches!(ch, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r') {
            return Ok(Some(ch));
        }
    }
    Ok(None)
}

pub(super) async fn same_segments(
    left: &str,
    right: &str,
    work: &mut impl Work,
) -> DictionaryResult<bool> {
    let (mut left, mut right) = (left.chars(), right.chars());
    loop {
        let a = next_non_whitespace(&mut left, work).await?;
        let b = next_non_whitespace(&mut right, work).await?;
        if a != b {
            return Ok(false);
        }
        if a.is_none() {
            return Ok(true);
        }
    }
}

pub(super) async fn space_split<'a>(
    value: &'a str,
    work: &mut impl Work,
) -> DictionaryResult<Vec<&'a str>> {
    if value.is_empty() {
        return Ok(vec![value]);
    }
    let mut parts = Vec::new();
    let mut start = 0;
    let mut separator = false;
    for (offset, byte) in value.bytes().enumerate() {
        work.records(offset).await?;
        if byte == b' ' {
            if !separator {
                parts.try_reserve(1)?;
                parts.push(&value[start..offset]);
            }
            separator = true;
            start = offset + 1;
        } else {
            separator = false;
        }
    }
    parts.try_reserve(1)?;
    parts.push(&value[start..]);
    while parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    Ok(parts)
}
