//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lucene CSV, Java line boundaries, and Japanese segmentation syntax.

use crate::kuromoji::error::{check_limit, invalid};
use crate::kuromoji::DictionaryResult;
use crate::morphology::io::vector;

pub(super) fn entries(source: &str, maximum: usize) -> DictionaryResult<Vec<Vec<String>>> {
    let mut entries = Vec::new();
    for line in source.split(['\r', '\n']) {
        if line.chars().all(|ch| ch <= '\u{20}')
            || (line.starts_with('#') && !line.contains(['\u{85}', '\u{2028}', '\u{2029}']))
        {
            continue;
        }
        check_limit("user dictionary entries", entries.len() + 1, maximum)?;
        let fields = csv(line)?;
        if fields.len() < 4 {
            return Err(invalid(
                "user dictionary",
                "CSV entry has fewer than four fields",
            ));
        }
        entries.try_reserve(1)?;
        entries.push(fields);
    }
    Ok(entries)
}

fn csv(line: &str) -> DictionaryResult<Vec<String>> {
    let mut fields = vector(4)?;
    let mut quoted = false;
    let mut start = 0;
    for (offset, ch) in line.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        }
        if ch == ',' && !quoted {
            if fields.len() < 4 {
                fields.push(unquote(&line[start..offset])?);
            }
            start = offset + 1;
        }
    }
    if quoted {
        return Err(invalid("user dictionary", "CSV quotes are unbalanced"));
    }
    // CSVUtil deliberately leaves its final field quoted and escaped.
    if fields.len() < 4 {
        fields.push(owned(&line[start..])?);
    }
    Ok(fields)
}

fn unquote(value: &str) -> DictionaryResult<String> {
    let value = match value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        Some(inner) if !inner.is_empty() && !inner.contains('"') => inner,
        _ => value,
    };
    let mut output = String::new();
    output.try_reserve_exact(value.len())?;
    let mut first = true;
    for part in value.split("\"\"") {
        if !first {
            output.push('"');
        }
        output.push_str(part);
        first = false;
    }
    Ok(output)
}

pub(super) fn owned(value: &str) -> DictionaryResult<String> {
    let mut output = String::new();
    output.try_reserve_exact(value.len())?;
    output.push_str(value);
    Ok(output)
}

pub(super) fn without_whitespace(value: &str) -> impl Iterator<Item = char> + '_ {
    value
        .chars()
        .filter(|ch| !matches!(ch, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r'))
}

pub(super) fn space_split(value: &str) -> DictionaryResult<Vec<&str>> {
    if value.is_empty() {
        return Ok(vec![value]);
    }
    let mut parts = Vec::new();
    let mut start = 0;
    let mut bytes = value.bytes().enumerate().peekable();
    while let Some((offset, byte)) = bytes.next() {
        if byte != b' ' {
            continue;
        }
        parts.try_reserve(1)?;
        parts.push(&value[start..offset]);
        start = offset + 1;
        while bytes.peek().is_some_and(|(_, byte)| *byte == b' ') {
            start = bytes.next().expect("peeked space").0 + 1;
        }
    }
    parts.try_reserve(1)?;
    parts.push(&value[start..]);
    while parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    Ok(parts)
}
