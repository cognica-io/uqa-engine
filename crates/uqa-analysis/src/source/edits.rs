//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered edit ranges mapping one character-filter output to its input.

use std::ops::Range;

use super::validate_utf8_range;
use crate::{AnalysisError, AnalysisResult};

pub(crate) struct TextEdit {
    pub range: Range<usize>,
    pub replacement: String,
}

#[derive(Debug, Clone)]
struct Segment {
    output: Range<usize>,
    input: Range<usize>,
    copied: bool,
}

#[derive(Debug, Clone)]
pub(super) struct EditMap {
    segments: Vec<Segment>,
    input_len: usize,
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
        };
        let mut cursor = 0;
        for edit in edits {
            map.append(
                &mut output,
                &input[cursor..edit.range.start],
                cursor..edit.range.start,
                true,
            );
            let copied = input[edit.range.clone()] == edit.replacement;
            cursor = edit.range.end;
            map.append(&mut output, &edit.replacement, edit.range, copied);
        }
        map.append(&mut output, &input[cursor..], cursor..input.len(), true);
        Ok(Some((output, map)))
    }

    fn append(&mut self, output: &mut String, text: &str, input: Range<usize>, copied: bool) {
        if text.is_empty() {
            return;
        }
        let start = output.len();
        output.push_str(text);
        if copied {
            if let Some(last) = self.segments.last_mut() {
                if last.copied && last.input.end == input.start {
                    last.input.end = input.end;
                    last.output.end = output.len();
                    return;
                }
            }
        }
        self.segments.push(Segment {
            output: start..output.len(),
            input,
            copied,
        });
    }

    pub(super) fn project(&self, range: Range<usize>) -> Range<usize> {
        let first = self
            .segments
            .partition_point(|segment| segment.output.end <= range.start);
        if range.is_empty() {
            let point = self.segments.get(first).map_or(self.input_len, |segment| {
                if segment.copied {
                    segment.input.start + range.start - segment.output.start
                } else {
                    segment.input.start
                }
            });
            return point..point;
        }

        let mut source = self.input_len..0;
        for segment in &self.segments[first..] {
            if segment.output.start >= range.end {
                break;
            }
            let mapped = if segment.copied {
                let start = range.start.max(segment.output.start) - segment.output.start;
                let end = range.end.min(segment.output.end) - segment.output.start;
                segment.input.start + start..segment.input.start + end
            } else {
                segment.input.clone()
            };
            source.start = source.start.min(mapped.start);
            source.end = source.end.max(mapped.end);
        }
        source
    }
}
