//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Java `BufferedReader`, Pattern comment removal, trim, and ASCII whitespace splitting.

use crate::nori::error::check_limit;
use crate::nori::DictionaryResult;

use std::borrow::Cow;
use std::ops::Range;

pub(super) struct Entry<'a> {
    pub original: Cow<'a, str>,
    fields: Vec<Range<usize>>,
}

impl Entry<'_> {
    pub fn surface(&self) -> &str {
        &self.original[self.fields[0].clone()]
    }
    pub fn labels(&self) -> impl ExactSizeIterator<Item = &str> {
        self.fields[1..]
            .iter()
            .map(|range| &self.original[range.clone()])
    }
}

fn whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}')
}
fn line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

// Java's default dot excludes Unicode line terminators, even inside a BufferedReader line.
fn remove_comment(line: &str) -> Cow<'_, str> {
    let effective_end = line
        .char_indices()
        .next_back()
        .filter(|(_, c)| line_terminator(*c))
        .map_or(line.len(), |(index, _)| index);
    let suffix_start = line[..effective_end]
        .char_indices()
        .filter(|(_, c)| line_terminator(*c))
        .map(|(index, c)| index + c.len_utf8())
        .next_back()
        .unwrap_or(0);
    let Some(index) = line[suffix_start..effective_end]
        .find('#')
        .map(|index| suffix_start + index)
    else {
        return Cow::Borrowed(line);
    };
    if effective_end == line.len() {
        return Cow::Borrowed(&line[..index]);
    }
    Cow::Owned(format!("{}{}", &line[..index], &line[effective_end..]))
}

pub(super) fn entries(source: &str, maximum_entries: usize) -> DictionaryResult<Vec<Entry<'_>>> {
    let mut entries = Vec::new();
    // CRLF produces an additional empty slice; Java trim discards it just as an empty line.
    for line in source.split(['\n', '\r']) {
        let line = remove_comment(line);
        if line.trim_matches(|c| c <= '\u{20}').is_empty() {
            continue;
        }
        let mut fields = Vec::new();
        let mut start = 0;
        let mut in_separator = false;
        for (index, c) in line.char_indices() {
            if whitespace(c) {
                if !in_separator {
                    fields.try_reserve(1)?;
                    fields.push(start..index);
                }
                in_separator = true;
                start = index + c.len_utf8();
            } else {
                in_separator = false;
            }
        }
        if start < line.len() {
            fields.try_reserve(1)?;
            fields.push(start..line.len());
        }
        check_limit(
            "user dictionary entries",
            entries.len() + 1,
            maximum_entries,
        )?;
        entries.try_reserve(1)?;
        entries.push(Entry {
            original: line,
            fields,
        });
    }
    Ok(entries)
}
