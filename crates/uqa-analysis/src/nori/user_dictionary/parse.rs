//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Java `BufferedReader`, Pattern comment removal, trim, and ASCII whitespace splitting.

use crate::morphology::work::{source, Work};
use crate::nori::error::check_limit;
use crate::nori::DictionaryResult;

use std::borrow::Cow;
use std::ops::Range;

pub(super) struct Entry<'a> {
    pub original: Cow<'a, str>,
    fields: Vec<Range<usize>>,
}

impl crate::morphology::work::ordering::Surface for Entry<'_> {
    fn surface(&self) -> &str {
        &self.original[self.fields[0].clone()]
    }
}

impl Entry<'_> {
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
async fn remove_comment<'a>(line: &'a str, work: &mut impl Work) -> DictionaryResult<Cow<'a, str>> {
    let effective_end = line
        .char_indices()
        .next_back()
        .filter(|(_, c)| line_terminator(*c))
        .map_or(line.len(), |(index, _)| index);
    let mut suffix_start = 0;
    for (count, (index, ch)) in line[..effective_end].char_indices().rev().enumerate() {
        work.records(count).await?;
        if line_terminator(ch) {
            suffix_start = index + ch.len_utf8();
            break;
        }
    }
    let Some(index) = crate::work::find_byte::<crate::morphology::error::DictionaryError>(
        &line[..effective_end],
        suffix_start,
        b'#',
        work,
    )
    .await?
    else {
        return Ok(Cow::Borrowed(line));
    };
    if effective_end == line.len() {
        return Ok(Cow::Borrowed(&line[..index]));
    }
    let mut output = String::new();
    output.try_reserve_exact(index + line.len() - effective_end)?;
    source::append(&mut output, &line[..index], work).await?;
    source::append(&mut output, &line[effective_end..], work).await?;
    Ok(Cow::Owned(output))
}

pub(super) async fn entries<'a>(
    source: &'a str,
    maximum_entries: usize,
    work: &mut impl Work,
) -> DictionaryResult<Vec<Entry<'a>>> {
    let mut entries = Vec::new();
    // CRLF produces an additional empty slice; Java trim discards it just as an empty line.
    let mut lines = source::Lines::new(source);
    while let Some(line) = lines.next(work).await? {
        let line = remove_comment(line, work).await?;
        if source::blank(&line, work).await? {
            continue;
        }
        let mut fields = Vec::new();
        let mut start = 0;
        let mut in_separator = false;
        for (count, (index, c)) in line.char_indices().enumerate() {
            work.records(count).await?;
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
