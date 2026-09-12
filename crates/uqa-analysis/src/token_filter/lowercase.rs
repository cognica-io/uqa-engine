//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Contextual lowercase with forward-only source traversal and reserved output.

use std::iter::Peekable;
use std::sync::OnceLock;

use regex_syntax::hir::{Class, ClassUnicode, HirKind};
use uqa_core::memory::{Budgeted, MemoryBudget};

use crate::{term::TermBuffer, AnalysisError, AnalysisResult, TokenTerm};

#[derive(Debug)]
pub(crate) struct CaseProperties {
    cased: ClassUnicode,
    ignorable: ClassUnicode,
}

pub(super) fn prepare() -> AnalysisResult<&'static CaseProperties> {
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

fn class(pattern: &str) -> Result<ClassUnicode, String> {
    let expression = regex_syntax::Parser::new()
        .parse(pattern)
        .map_err(|error| error.to_string())?;
    match expression.into_kind() {
        HirKind::Class(Class::Unicode(class)) => Ok(class),
        _ => Err("lowercase context property is not a Unicode class".into()),
    }
}

impl CaseProperties {
    fn cased(&self, character: char) -> bool {
        contains(&self.cased, character)
    }
    fn ignorable(&self, character: char) -> bool {
        contains(&self.ignorable, character)
    }
}

fn contains(class: &ClassUnicode, character: char) -> bool {
    class
        .ranges()
        .binary_search_by(|range| {
            if range.end() < character {
                std::cmp::Ordering::Less
            } else if range.start() > character {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

pub(super) fn lower_budgeted(
    input: &TokenTerm,
    properties: &CaseProperties,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<TokenTerm>> {
    poll()?;
    let mut output = TermBuffer::new(input, budget);
    let mut following = input.characters().enumerate().peekable();
    let mut preceded_cased = false;
    for (index, character) in input.characters().enumerate() {
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
