//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restricted CJK width folding with immediate emission after a voiced-mark combination.

use unicode_normalization::char::{compose, decompose_compatible};
use uqa_core::memory::MemoryBudget;

use crate::source::EditBuilder;
use crate::{AnalysisResult, FilteredText};

fn fold(character: char) -> char {
    if matches!(character, '\u{ff01}'..='\u{ff5e}' | '\u{ff65}'..='\u{ff9f}') {
        // Each restricted width decomposition is exactly one scalar; other compatibility forms stay unchanged.
        let mut folded = character;
        decompose_compatible(character, |value| folded = value);
        folded
    } else {
        character
    }
}

pub(super) fn replace_width(
    text: &mut FilteredText<'_>,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    poll()?;
    let edited = {
        let input = text.as_str();
        let mut builder = EditBuilder::new(input, budget, poll);
        let mut characters = input.char_indices().peekable();
        while let Some((start, original)) = characters.next() {
            builder.check()?;
            let mut folded = fold(original);
            let mut end = start + original.len_utf8();
            if matches!(folded, '\u{30a6}'..='\u{30fd}') {
                if let Some(&(_, mark @ ('\u{ff9e}' | '\u{ff9f}'))) = characters.peek() {
                    if let Some(combined) = compose(folded, fold(mark)) {
                        folded = combined;
                        end += mark.len_utf8();
                        characters.next();
                    }
                }
            }
            if folded != original || end != start + original.len_utf8() {
                let mut buffer = [0; 4];
                builder.edit(
                    start..end,
                    std::iter::once(&*folded.encode_utf8(&mut buffer)),
                )?;
            }
        }
        builder.finish()?
    };
    text.apply_edited(edited, budget, poll)
}

#[cfg(test)]
mod tests;
