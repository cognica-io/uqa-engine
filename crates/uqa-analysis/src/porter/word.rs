//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A reserved word stores consonant state without recursively revisiting preceding y's.

use std::ops::Deref;

use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryError};

use super::Character;
use crate::AnalysisResult;

pub(super) struct Word<'a> {
    characters: BudgetedVec<Character>,
    consonants: BudgetedVec<bool>,
    poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
}

impl<'a> Word<'a> {
    pub fn new(
        characters: impl Iterator<Item = Character> + Clone,
        budget: &MemoryBudget,
        poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Self> {
        poll()?;
        let mut length = 0usize;
        for (index, _) in characters.clone().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            length = length.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
        }
        let mut word = Self {
            characters: BudgetedVec::new(budget),
            consonants: BudgetedVec::new(budget),
            poll,
        };
        word.characters.reserve(length)?;
        word.consonants.reserve(length)?;
        for (index, character) in characters.enumerate() {
            if index % 1024 == 0 {
                word.check()?;
            }
            word.push(character)?;
        }
        word.check()?;
        Ok(word)
    }

    pub fn check(&mut self) -> AnalysisResult<()> {
        (self.poll)()
    }

    pub fn push(&mut self, character: Character) -> AnalysisResult<()> {
        let consonant = if character == 'y' {
            self.consonants.last().is_none_or(|previous| !previous)
        } else {
            !character.is_one_of(&['a', 'e', 'i', 'o', 'u'])
        };
        self.characters.reserve(1)?;
        self.consonants.reserve(1)?;
        self.characters.push(character)?;
        self.consonants.push(consonant)?;
        Ok(())
    }

    pub fn truncate(&mut self, length: usize) {
        self.characters.truncate(length);
        self.consonants.truncate(length);
    }

    pub fn replace_suffix(&mut self, length: usize, replacement: &str) -> AnalysisResult<()> {
        self.truncate(self.len() - length);
        for character in replacement.chars() {
            self.push(character.into())?;
        }
        Ok(())
    }

    pub fn ends_with(&self, suffix: &str) -> bool {
        debug_assert!(suffix.is_ascii());
        suffix.len() <= self.len()
            && self[self.len() - suffix.len()..]
                .iter()
                .zip(suffix.chars())
                .all(|(&left, right)| left == right)
    }

    /// Count vowel-to-consonant transitions in the retained prefix.
    pub fn measure(&mut self, length: usize) -> AnalysisResult<usize> {
        let mut previous_vowel = false;
        let mut count = 0;
        for index in 0..length {
            if index % 1024 == 0 {
                self.check()?;
            }
            let consonant = self.consonants[index];
            if previous_vowel && consonant {
                count += 1;
            }
            previous_vowel = !consonant;
        }
        Ok(count)
    }

    pub fn has_vowel(&mut self, length: usize) -> AnalysisResult<bool> {
        for index in 0..length {
            if index % 1024 == 0 {
                self.check()?;
            }
            if !self.consonants[index] {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn double_consonant(&self) -> bool {
        self.len() >= 2
            && self[self.len() - 1] == self[self.len() - 2]
            && self.consonants[self.len() - 1]
    }

    pub fn cvc(&self, length: usize) -> bool {
        length >= 3
            && self.consonants[length - 1]
            && !self.consonants[length - 2]
            && self.consonants[length - 3]
            && !self[length - 1].is_one_of(&['w', 'x', 'y'])
    }
}

impl Deref for Word<'_> {
    type Target = [Character];

    fn deref(&self) -> &Self::Target {
        &self.characters
    }
}
