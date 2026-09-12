//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered edit ranges mapping one character-filter output to its input.

use std::ops::Range;

use super::validate_utf8_range;
use crate::{AnalysisError, AnalysisResult, SourceOffsets};

pub(crate) struct TextEdit {
    pub range: Range<usize>,
    pub replacement: String,
}

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

impl EditMap {
    pub(super) fn apply(
        input: &str,
        edits: Vec<TextEdit>,
    ) -> AnalysisResult<Option<(String, Self)>> {
        let mut previous_end = 0;
        let mut changed = false;
        for edit in &edits {
            validate_utf8_range(input, &edit.range)?;
            if edit.range.start < previous_end {
                return Err(AnalysisError::OverlappingTextEdits);
            }
            previous_end = edit.range.end;
            changed |= input[edit.range.clone()] != edit.replacement;
        }
        if !changed {
            return Ok(None);
        }

        let mut output = String::new();
        let mut map = Self {
            segments: Vec::new(),
            input_len: input.len(),
            input_utf16_len: 0,
            output_utf16_len: 0,
        };
        let mut cursor = 0;
        let mut cursor_utf16 = 0;
        for edit in edits {
            let copied_text = &input[cursor..edit.range.start];
            let start_utf16 = cursor_utf16 + copied_text.encode_utf16().count();
            map.append(
                &mut output,
                copied_text,
                SourceOffsets {
                    utf8: cursor..edit.range.start,
                    utf16: cursor_utf16..start_utf16,
                },
                true,
            );
            let replaced = &input[edit.range.clone()];
            let copied = replaced == edit.replacement;
            cursor = edit.range.end;
            cursor_utf16 = start_utf16 + replaced.encode_utf16().count();
            map.append(
                &mut output,
                &edit.replacement,
                SourceOffsets {
                    utf8: edit.range,
                    utf16: start_utf16..cursor_utf16,
                },
                copied,
            );
        }
        let tail = &input[cursor..];
        map.input_utf16_len = cursor_utf16 + tail.encode_utf16().count();
        map.append(
            &mut output,
            tail,
            SourceOffsets {
                utf8: cursor..input.len(),
                utf16: cursor_utf16..map.input_utf16_len,
            },
            true,
        );
        Ok(Some((output, map)))
    }

    fn append(&mut self, output: &mut String, text: &str, input: SourceOffsets, copied: bool) {
        if text.is_empty() {
            return;
        }
        let start = output.len();
        let start_utf16 = self.output_utf16_len;
        self.output_utf16_len += if copied {
            input.utf16.len()
        } else {
            text.encode_utf16().count()
        };
        output.push_str(text);
        if copied {
            if let Some(last) = self.segments.last_mut() {
                if last.copied && last.input.utf8.end == input.utf8.start {
                    last.input.utf8.end = input.utf8.end;
                    last.input.utf16.end = input.utf16.end;
                    last.output.utf8.end = output.len();
                    last.output.utf16.end = self.output_utf16_len;
                    return;
                }
            }
        }
        self.segments.push(Segment {
            output: SourceOffsets {
                utf8: start..output.len(),
                utf16: start_utf16..self.output_utf16_len,
            },
            input,
            copied,
        });
    }

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
