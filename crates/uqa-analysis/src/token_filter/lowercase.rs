//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Contextual lowercase with forward-only source traversal and reserved output.

use std::iter::Peekable;
use std::sync::OnceLock;

use regex_syntax::hir::ClassUnicode;
use uqa_core::memory::{Budgeted, BudgetedString, MemoryBudget};

use crate::character_class::{class, contains};
use crate::{term::TermBuffer, AnalysisError, AnalysisResult, TokenTerm};

#[derive(Debug)]
pub(crate) struct CaseProperties {
    cased: ClassUnicode,
    ignorable: ClassUnicode,
}

pub(crate) fn prepare() -> AnalysisResult<&'static CaseProperties> {
    static PROPERTIES: OnceLock<Result<CaseProperties, String>> = OnceLock::new();
    PROPERTIES
        .get_or_init(|| {
            Ok(CaseProperties {
                cased: class(r"\p{Cased}")?,
                ignorable: class(r"\p{Case_Ignorable}")?,
            })
        })
        .as_ref()
        .map_err(|message| AnalysisError::BuiltInRegex {
            component: "lowercase context properties",
            message: message.clone(),
        })
}

impl CaseProperties {
    fn cased(&self, character: char) -> bool {
        contains(&self.cased, character)
    }
    fn ignorable(&self, character: char) -> bool {
        contains(&self.ignorable, character)
    }
}

pub(super) fn lower_budgeted(
    input: &TokenTerm,
    properties: &CaseProperties,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<TokenTerm>> {
    lower(
        input.characters(),
        TermBuffer::new(input, budget),
        properties,
        poll,
    )
}

pub(crate) fn lower_text_budgeted(
    input: &str,
    properties: &CaseProperties,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<TokenTerm>> {
    lower(
        input.chars().map(Ok),
        TermBuffer::Unicode(BudgetedString::new(budget)),
        properties,
        poll,
    )
}

fn lower(
    characters: impl Iterator<Item = Result<char, u16>> + Clone,
    mut output: TermBuffer,
    properties: &CaseProperties,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<TokenTerm>> {
    poll()?;
    let mut following = characters.clone().enumerate().peekable();
    let mut preceded_cased = false;
    for (index, character) in characters.enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        let Ok(character) = character else {
            output.push(character)?;
            preceded_cased = false;
            continue;
        };
        if character == 'Σ'
            && preceded_cased
            && !following_cased(&mut following, index, properties, poll)?
        {
            output.push(Ok('ς'))?;
        } else {
            for lowered in character.to_lowercase() {
                output.push(Ok(lowered))?;
            }
        }
        if !properties.ignorable(character) {
            preceded_cased = properties.cased(character);
        }
    }
    output.finish(poll)
}

fn following_cased(
    following: &mut Peekable<impl Iterator<Item = (usize, Result<char, u16>)>>,
    current: usize,
    properties: &CaseProperties,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<bool> {
    loop {
        let Some(&(index, character)) = following.peek() else {
            return Ok(false);
        };
        if index % 1024 == 0 {
            poll()?;
        }
        if index > current {
            match character {
                Ok(character) if properties.ignorable(character) => {}
                Ok(character) => return Ok(properties.cased(character)),
                Err(_) => return Ok(false),
            }
        }
        following.next();
    }
}

#[cfg(test)]
mod tests;
