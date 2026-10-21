//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Contextual lowercase with forward-only source traversal and reserved output.

use std::iter::Peekable;

use icu_casemap::CaseMapper;
use icu_properties::CodePointSetDataBorrowed;
use uqa_core::memory::{Budgeted, BudgetedString, MemoryBudget};
use writeable::Writeable;

use crate::{term::TermBuffer, AnalysisError, AnalysisResult, TokenTerm};

#[derive(Debug)]
pub(crate) struct CaseProperties {
    profile: crate::unicode::Profile,
    cased: CodePointSetDataBorrowed<'static>,
    ignorable: CodePointSetDataBorrowed<'static>,
}

pub(crate) fn prepare() -> &'static CaseProperties {
    prepare_with_profile(crate::unicode::Profile::default())
}

pub(crate) fn prepare_with_profile(profile: crate::unicode::Profile) -> &'static CaseProperties {
    use crate::unicode::Profile;
    static UNICODE_16: CaseProperties = CaseProperties {
        profile: Profile::Unicode16,
        cased: crate::unicode::CASED,
        ignorable: crate::unicode::CASE_IGNORABLE,
    };
    // Historical Rust 17 mappings used regex-syntax's Unicode 16 context classes.
    static UNICODE_17: CaseProperties = CaseProperties {
        profile: Profile::Unicode17,
        cased: crate::unicode::CASED,
        ignorable: crate::unicode::CASE_IGNORABLE,
    };
    match profile {
        Profile::Unicode16 => &UNICODE_16,
        Profile::Unicode17 => &UNICODE_17,
    }
}

impl CaseProperties {
    fn cased(&self, character: char) -> bool {
        self.cased.contains(character)
    }
    fn ignorable(&self, character: char) -> bool {
        self.ignorable.contains(character)
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
            lower_character(character, properties.profile, &mut output)?;
        }
        if !properties.ignorable(character) {
            preceded_cased = properties.cased(character);
        }
    }
    output.finish(poll)
}

fn lower_character(
    character: char,
    profile: crate::unicode::Profile,
    output: &mut TermBuffer,
) -> AnalysisResult<()> {
    // Stream full mappings (including expansions) into the caller's admitted buffer.
    struct Writer<'a> {
        output: &'a mut TermBuffer,
        error: Option<AnalysisError>,
    }
    impl std::fmt::Write for Writer<'_> {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            for character in text.chars() {
                self.output.push(Ok(character)).map_err(|error| {
                    self.error = Some(error);
                    std::fmt::Error
                })?;
            }
            Ok(())
        }
    }
    if character.is_ascii() {
        return output.push(Ok(character.to_ascii_lowercase()));
    }
    if let Some(lower) = profile.lowercase_override(character) {
        return output.push(Ok(lower));
    }
    let mut writer = Writer {
        output,
        error: None,
    };
    let mut bytes = [0; 4];
    let result = CaseMapper::new()
        .lowercase(
            character.encode_utf8(&mut bytes),
            &icu_locale_core::LanguageIdentifier::UNKNOWN,
        )
        .write_to(&mut writer);
    result.map_err(|_| {
        writer
            .error
            .expect("lowercase output failed its reservation")
    })
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
