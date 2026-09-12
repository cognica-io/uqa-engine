//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered edit ranges mapping one character-filter output to its input.

use std::ops::Range;
use uqa_core::memory::{Budgeted, BudgetedString, BudgetedVec, MemoryBudget, MemoryError};

use super::validate_utf8_range;
use crate::{AnalysisError, AnalysisResult, SourceOffsets};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    output: SourceOffsets,
    input: SourceOffsets,
    copied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EditMap {
    segments: Vec<Segment>,
    input_len: usize,
    input_utf16_len: usize,
    output_utf16_len: usize,
}

pub(crate) struct EditedText {
    pub(super) text: Budgeted<String>,
    pub(super) map: Budgeted<EditMap>,
}

/// One ordered edit pass; unchanged replacements never materialize an output buffer.
pub(crate) struct EditBuilder<'a, 'c> {
    input: &'a str,
    output: BudgetedString,
    segments: BudgetedVec<Segment>,
    cursor: usize,
    cursor_utf16: usize,
    previous_end: usize,
    output_utf16: usize,
    changed: bool,
    poll: &'c mut dyn FnMut() -> AnalysisResult<()>,
}

impl<'a, 'c> EditBuilder<'a, 'c> {
    pub(crate) fn new(
        input: &'a str,
        budget: &MemoryBudget,
        poll: &'c mut dyn FnMut() -> AnalysisResult<()>,
    ) -> Self {
        Self {
            input,
            output: BudgetedString::new(budget),
            segments: BudgetedVec::new(budget),
            cursor: 0,
            cursor_utf16: 0,
            previous_end: 0,
            output_utf16: 0,
            changed: false,
            poll,
        }
    }

    pub(crate) fn edit<'t>(
        &mut self,
        range: Range<usize>,
        replacement: impl Iterator<Item = &'t str> + Clone,
    ) -> AnalysisResult<()> {
        (self.poll)()?;
        validate_utf8_range(self.input, &range)?;
        if range.start < self.previous_end {
            return Err(AnalysisError::OverlappingTextEdits);
        }
        self.previous_end = range.end;
        if self.identical(&range, replacement.clone())? {
            return Ok(());
        }
        self.changed = true;
        let input = self.input;
        let copied = &input[self.cursor..range.start];
        let start_utf16 = self.cursor_utf16 + count_utf16(copied, self.poll)?;
        self.append(
            std::iter::once(copied),
            SourceOffsets {
                utf8: self.cursor..range.start,
                utf16: self.cursor_utf16..start_utf16,
            },
            true,
        )?;
        let end_utf16 = start_utf16 + count_utf16(&input[range.clone()], self.poll)?;
        self.cursor = range.end;
        self.cursor_utf16 = end_utf16;
        self.append(
            replacement,
            SourceOffsets {
                utf8: range,
                utf16: start_utf16..end_utf16,
            },
            false,
        )
    }

    pub(crate) fn check(&mut self) -> AnalysisResult<()> {
        (self.poll)()
    }

    fn identical<'t>(
        &mut self,
        range: &Range<usize>,
        replacement: impl Iterator<Item = &'t str>,
    ) -> AnalysisResult<bool> {
        let mut offset = range.start;
        for fragment in replacement {
            (self.poll)()?;
            let Some(end) = offset
                .checked_add(fragment.len())
                .filter(|end| *end <= range.end)
            else {
                return Ok(false);
            };
            for (left, right) in self.input.as_bytes()[offset..end]
                .chunks(1024)
                .zip(fragment.as_bytes().chunks(1024))
            {
                (self.poll)()?;
                if left != right {
                    return Ok(false);
                }
            }
            offset = end;
        }
        Ok(offset == range.end)
    }

    fn append<'t>(
        &mut self,
        fragments: impl Iterator<Item = &'t str>,
        input: SourceOffsets,
        copied: bool,
    ) -> AnalysisResult<()> {
        let start = self.output.len();
        let start_utf16 = self.output_utf16;
        for fragment in fragments {
            (self.poll)()?;
            self.output.reserve(fragment.len())?;
            for (index, character) in fragment.chars().enumerate() {
                if index % 1024 == 0 {
                    (self.poll)()?;
                }
                self.output.push(character)?;
                self.output_utf16 = self
                    .output_utf16
                    .checked_add(character.len_utf16())
                    .ok_or(MemoryError::SizeOverflow)?;
            }
        }
        if self.output.len() == start {
            return Ok(());
        }
        if copied {
            if let Some(last) = self.segments.last_mut() {
                if last.copied && last.input.utf8.end == input.utf8.start {
                    last.input.utf8.end = input.utf8.end;
                    last.input.utf16.end = input.utf16.end;
                    last.output.utf8.end = self.output.len();
                    last.output.utf16.end = self.output_utf16;
                    return Ok(());
                }
            }
        }
        self.segments.push(Segment {
            output: SourceOffsets {
                utf8: start..self.output.len(),
                utf16: start_utf16..self.output_utf16,
            },
            input,
            copied,
        })?;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> AnalysisResult<Option<EditedText>> {
        (self.poll)()?;
        if !self.changed {
            return Ok(None);
        }
        let input = self.input;
        let tail = &input[self.cursor..];
        let input_utf16_len = self.cursor_utf16 + count_utf16(tail, self.poll)?;
        self.append(
            std::iter::once(tail),
            SourceOffsets {
                utf8: self.cursor..input.len(),
                utf16: self.cursor_utf16..input_utf16_len,
            },
            true,
        )?;
        let (text, text_memory) = self.output.into_parts();
        let (segments, map_memory) = self.segments.into_parts();
        Ok(Some(EditedText {
            text: Budgeted::new(text, text_memory),
            map: Budgeted::new(
                EditMap {
                    segments,
                    input_len: input.len(),
                    input_utf16_len,
                    output_utf16_len: self.output_utf16,
                },
                map_memory,
            ),
        }))
    }
}

fn count_utf16(text: &str, poll: &mut dyn FnMut() -> AnalysisResult<()>) -> AnalysisResult<usize> {
    let mut units = 0;
    for (index, character) in text.chars().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        units += character.len_utf16();
    }
    Ok(units)
}

impl EditMap {
    pub(super) fn project(&self, range: Range<usize>) -> Range<usize> {
        self.project_in(range, |offsets| &offsets.utf8, self.input_len)
    }

    pub(super) fn project_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.project_in(range, |offsets| &offsets.utf16, self.input_utf16_len)
    }

    fn project_in(
        &self,
        range: Range<usize>,
        select: fn(&SourceOffsets) -> &Range<usize>,
        input_len: usize,
    ) -> Range<usize> {
        let first = self
            .segments
            .partition_point(|segment| select(&segment.output).end <= range.start);
        if range.is_empty() {
            let point = self.segments.get(first).map_or(input_len, |segment| {
                if segment.copied {
                    select(&segment.input).start + range.start - select(&segment.output).start
                } else {
                    select(&segment.input).start
                }
            });
            return point..point;
        }

        let mut source = input_len..0;
        for segment in &self.segments[first..] {
            let output = select(&segment.output);
            let input = select(&segment.input);
            if output.start >= range.end {
                break;
            }
            let mapped = if segment.copied {
                let start = range.start.max(output.start) - output.start;
                let end = range.end.min(output.end) - output.start;
                input.start + start..input.start + end
            } else {
                input.clone()
            };
            source.start = source.start.min(mapped.start);
            source.end = source.end.max(mapped.end);
        }
        source
    }
}
