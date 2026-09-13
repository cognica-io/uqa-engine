//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fold individual scalars without allocating decomposition strings or raw-term segments.

use unicode_normalization::char::decompose_compatible;
use uqa_core::memory::{Budgeted, MemoryBudget};

use crate::{term::TermBuffer, AnalysisResult, TokenTerm};

pub(super) fn fold_budgeted(
    input: &TokenTerm,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<TokenTerm>> {
    poll()?;
    let mut output = TermBuffer::new(input, budget);
    for (index, character) in input.characters().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        match character {
            Ok(character) if !character.is_ascii() => {
                let mut emitted = false;
                let mut failure = None;
                decompose_compatible(character, |part| {
                    if part.is_ascii() && failure.is_none() {
                        emitted = true;
                        failure = output.push(Ok(part)).err();
                    }
                });
                if let Some(error) = failure {
                    return Err(error);
                }
                if !emitted {
                    output.push(Ok(character))?;
                }
            }
            character => output.push(character)?,
        }
    }
    output.finish(poll)
}

#[cfg(test)]
mod tests;
