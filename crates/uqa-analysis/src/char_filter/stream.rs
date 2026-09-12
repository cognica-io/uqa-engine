//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered character edits write directly into reserved text and source-map buffers.

use regex::Regex;
use uqa_core::memory::{MemoryBudget, MemoryError};

use super::replacement::Replacement;
use crate::source::EditBuilder;
use crate::{AnalysisResult, FilteredText};

pub(super) fn replace_literal(
    text: &mut FilteredText<'_>,
    old: &str,
    new: &str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    poll()?;
    let edited = {
        let input = text.as_str();
        let mut builder = EditBuilder::new(input, budget, poll);
        for (start, matched) in input.match_indices(old) {
            builder.edit(start..start + matched.len(), std::iter::once(new))?;
        }
        builder.finish()?
    };
    text.apply_edited(edited, budget, poll)
}

pub(super) fn replace_pattern(
    text: &mut FilteredText<'_>,
    pattern: &Regex,
    replacement: &Replacement<'_>,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    poll()?;
    // CaptureLocations owns two pointer-sized slots per group in the pinned regex implementation.
    let mut capture_memory = budget.empty_reservation();
    let mut locations = if replacement.uses_captures() {
        let bytes = pattern
            .captures_len()
            .checked_mul(2 * size_of::<usize>())
            .ok_or(MemoryError::SizeOverflow)?;
        capture_memory.grow(bytes)?;
        Some(pattern.capture_locations())
    } else {
        None
    };
    let edited = {
        let input = text.as_str();
        let mut builder = EditBuilder::new(input, budget, poll);
        let mut start = 0;
        let mut last_end = None;
        loop {
            builder.check()?;
            let matched = if let Some(locations) = &mut locations {
                pattern.captures_read_at(locations, input, start)
            } else {
                pattern.find_at(input, start)
            };
            let Some(matched) = matched else {
                break;
            };
            if matched.is_empty() && Some(matched.end()) == last_end {
                if start == input.len() {
                    break;
                }
                start += 1;
                continue;
            }
            builder.edit(
                matched.range(),
                replacement.fragments(locations.as_ref(), input),
            )?;
            start = matched.end();
            last_end = Some(start);
        }
        builder.finish()?
    };
    drop(locations);
    drop(capture_memory);
    text.apply_edited(edited, budget, poll)
}
