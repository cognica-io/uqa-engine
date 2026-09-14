//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render borrowed source windows directly into one reserved output buffer.

use super::HighlightOptions;
use crate::{AnalysisError, AnalysisResult};
use uqa_core::memory::{Budgeted, BudgetedString, BudgetedVec, MemoryBudget, MemoryError};
use uqa_core::ordering::sort_by_with_control as sort_by;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Span {
    pub start: usize,
    pub end: usize,
    character_start: usize,
    character_end: usize,
}
impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            character_start: 0,
            character_end: 0,
        }
    }
}

struct Cursor<'a> {
    characters: std::str::CharIndices<'a>,
    position: usize,
    byte: usize,
}
impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            characters: text.char_indices(),
            position: 0,
            byte: 0,
        }
    }
    fn next(
        &mut self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Option<(usize, usize, char)>> {
        if self.position.is_multiple_of(1024) {
            poll()?;
        }
        let Some((byte, character)) = self.characters.next() else {
            return Ok(None);
        };
        let position = self.position;
        self.position += 1;
        self.byte = byte + character.len_utf8();
        Ok(Some((position, byte, character)))
    }
    fn seek(
        &mut self,
        position: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        poll()?;
        while self.position < position && self.next(poll)?.is_some() {}
        Ok(self.byte)
    }
    fn seek_byte(
        &mut self,
        byte: usize,
        length: usize,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        poll()?;
        while self.byte < byte && self.next(poll)?.is_some() {}
        if self.byte != byte {
            return Err(AnalysisError::InvalidTextOffset {
                coordinate: "UTF-8",
                offset: byte,
                length,
            });
        }
        Ok(self.position)
    }
}

#[derive(Clone, Copy)]
struct Cluster {
    first: usize,
    last: usize,
}
struct Window {
    start: usize,
    end: usize,
    first: usize,
    last: usize,
    prefix: bool,
    suffix: bool,
}

pub(super) fn merge_spans(
    spans: &mut BudgetedVec<Span>,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    sort_by(spans, poll, |left, right, _| {
        Ok((left.start, left.end).cmp(&(right.start, right.end)))
    })?;
    let mut retained = 0;
    for index in 0..spans.len() {
        poll()?;
        let span = spans[index];
        if retained > 0 && span.start < spans[retained - 1].end {
            spans[retained - 1].end = spans[retained - 1].end.max(span.end);
        } else {
            spans[retained] = span;
            retained += 1;
        }
    }
    spans.truncate(retained);
    Ok(())
}

pub(super) fn render(
    text: &str,
    mut spans: BudgetedVec<Span>,
    opts: &HighlightOptions,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    poll()?;
    if spans.is_empty() {
        drop(spans);
        return if opts.max_fragments == 0 {
            crate::allocation::copy_text(text, budget, poll)
        } else {
            prefix(text, opts.fragment_size, budget, poll)
        };
    }
    let mut output = BudgetedString::new(budget);
    if opts.max_fragments == 0 {
        output.reserve(wrapped_size(text.len(), spans.len(), opts)?)?;
        write_window(&mut output, text, 0, text.len(), &spans, opts, poll)?;
    } else {
        let mut cursor = Cursor::new(text);
        for span in &mut *spans {
            span.character_start = cursor.seek_byte(span.start, text.len(), poll)?;
            span.character_end = cursor.seek_byte(span.end, text.len(), poll)?;
        }
        cursor.seek(usize::MAX, poll)?;
        let windows = windows(text, &spans, cursor.position, opts, budget, poll)?;
        let mut required = windows.len().saturating_sub(1);
        for window in &*windows {
            poll()?;
            let bytes = wrapped_size(window.end - window.start, window.last - window.first, opts)?;
            required = required
                .checked_add(bytes)
                .and_then(|size| {
                    size.checked_add(
                        3 * usize::from(window.prefix) + 3 * usize::from(window.suffix),
                    )
                })
                .ok_or(MemoryError::SizeOverflow)?;
        }
        output.reserve(required)?;
        for (index, window) in windows.iter().enumerate() {
            poll()?;
            if index > 0 {
                output.push(' ')?;
            }
            if window.prefix {
                output.push_str("...")?;
            }
            write_window(
                &mut output,
                text,
                window.start,
                window.end,
                &spans[window.first..window.last],
                opts,
                poll,
            )?;
            if window.suffix {
                output.push_str("...")?;
            }
        }
    }
    poll()?;
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}

fn wrapped_size(bytes: usize, matches: usize, opts: &HighlightOptions) -> AnalysisResult<usize> {
    opts.start_tag
        .len()
        .checked_add(opts.end_tag.len())
        .and_then(|tags| tags.checked_mul(matches))
        .and_then(|tags| bytes.checked_add(tags))
        .ok_or_else(|| MemoryError::SizeOverflow.into())
}

fn append(
    output: &mut BudgetedString,
    text: &str,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    poll()?;
    for (index, character) in text.chars().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        output.push(character)?;
    }
    Ok(())
}

fn write_window(
    output: &mut BudgetedString,
    text: &str,
    start: usize,
    end: usize,
    spans: &[Span],
    opts: &HighlightOptions,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    let mut previous = start;
    for span in spans {
        poll()?;
        append(output, &text[previous..span.start], poll)?;
        append(output, &opts.start_tag, poll)?;
        append(output, &text[span.start..span.end], poll)?;
        append(output, &opts.end_tag, poll)?;
        previous = span.end;
    }
    append(output, &text[previous..end], poll)
}

fn prefix(
    text: &str,
    size: usize,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    let mut cursor = Cursor::new(text);
    let end = cursor.seek(size, poll)?;
    let mut output = BudgetedString::new(budget);
    output.reserve(
        end.checked_add(if end < text.len() { 3 } else { 0 })
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    append(&mut output, &text[..end], poll)?;
    if end < text.len() {
        output.push_str("...")?;
    }
    poll()?;
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory))
}

fn windows(
    text: &str,
    spans: &[Span],
    total: usize,
    opts: &HighlightOptions,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<BudgetedVec<Window>> {
    let half = (opts.fragment_size / 2).max(1);
    let mut count = 1;
    for pair in spans.windows(2) {
        poll()?;
        if pair[1]
            .character_start
            .saturating_sub(pair[0].character_end)
            > half
        {
            count += 1;
        }
    }
    let mut clusters = BudgetedVec::new(budget);
    clusters.reserve(count)?;
    let mut first = 0;
    for index in 1..spans.len() {
        poll()?;
        if spans[index]
            .character_start
            .saturating_sub(spans[index - 1].character_end)
            > half
        {
            clusters.push(Cluster { first, last: index })?;
            first = index;
        }
    }
    clusters.push(Cluster {
        first,
        last: spans.len(),
    })?;
    // Explicit source order preserves the preceding stable density tie break.
    sort_by(&mut clusters, poll, |left, right, _| {
        Ok((std::cmp::Reverse(left.last - left.first), left.first)
            .cmp(&(std::cmp::Reverse(right.last - right.first), right.first)))
    })?;
    clusters.truncate(opts.max_fragments);
    sort_by(&mut clusters, poll, |left, right, _| {
        Ok(left.first.cmp(&right.first))
    })?;
    let mut output = BudgetedVec::new(budget);
    output.reserve(clusters.len())?;
    let (mut starts, mut ends) = (Cursor::new(text), Cursor::new(text));
    for cluster in &*clusters {
        poll()?;
        output.push(window(
            cluster,
            spans,
            total,
            half,
            &mut starts,
            &mut ends,
            poll,
        )?)?;
    }
    Ok(output)
}

fn window(
    cluster: &Cluster,
    spans: &[Span],
    total: usize,
    half: usize,
    starts: &mut Cursor<'_>,
    ends: &mut Cursor<'_>,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Window> {
    let first = spans[cluster.first].character_start;
    let last = spans[cluster.last - 1].character_end;
    let centre = first + (last - first) / 2;
    let mut focus = cluster.first;
    let mut distance = usize::MAX;
    for (index, span) in spans
        .iter()
        .enumerate()
        .take(cluster.last)
        .skip(cluster.first)
    {
        poll()?;
        let midpoint = span.character_start + (span.character_end - span.character_start) / 2;
        let next = midpoint.abs_diff(centre);
        if next < distance {
            focus = index;
            distance = next;
        }
    }
    let focus = spans[focus];
    let mut start = centre.saturating_sub(half).min(focus.character_start);
    let mut end = centre
        .saturating_add(half)
        .min(total)
        .max(focus.character_end);
    let mut start_byte = starts.seek(start, poll)?;
    if start > 0 {
        let limit = start.saturating_add(30).min(focus.character_start);
        while starts.position < limit {
            let Some((_, _, character)) = starts.next(poll)? else {
                break;
            };
            if character.is_whitespace() {
                start = starts.position;
                start_byte = starts.byte;
                break;
            }
        }
    }
    // Separate cursors permit overlapping context windows without rescanning any source prefix.
    let mut end_byte;
    if end < total {
        let lower = end.saturating_sub(30).max(focus.character_end);
        ends.seek(lower, poll)?;
        let mut whitespace = None;
        while ends.position < end {
            let Some((position, byte, character)) = ends.next(poll)? else {
                break;
            };
            if character.is_whitespace() {
                whitespace = Some((position, byte));
            }
        }
        end_byte = ends.byte;
        if let Some((position, byte)) = whitespace {
            end = position;
            end_byte = byte;
        }
    } else {
        end_byte = ends.seek(end, poll)?;
    }
    let mut first = cluster.first;
    while first < cluster.last && spans[first].character_start < start {
        poll()?;
        first += 1;
    }
    let mut last = first;
    while last < cluster.last && spans[last].character_end <= end {
        poll()?;
        last += 1;
    }
    Ok(Window {
        start: start_byte,
        end: end_byte,
        first,
        last,
        prefix: start > 0,
        suffix: end < total,
    })
}
