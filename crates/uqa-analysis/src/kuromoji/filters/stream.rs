//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese attributes adapt the existing reserved stream without coupling to Korean policy.

use super::super::{KuromojiOutput, KuromojiToken};
use crate::morphology::filter::{FilterStream, FilterToken, Work};
use crate::{AnalysisResult, AnalysisToken};
use uqa_core::memory::{Budgeted, MemoryReservation};

pub(super) trait JapaneseToken: FilterToken {
    fn attributes(&self) -> [Option<&str>; 6];
    fn part_of_speech(&self) -> AnalysisResult<Option<&str>>;
    fn reading(&self) -> AnalysisResult<Option<&str>>;
    fn base_form(&self) -> Option<&str> {
        self.attributes()[1]
    }
}
impl JapaneseToken for KuromojiToken {
    fn reading(&self) -> AnalysisResult<Option<&str>> {
        self.errors.check(2)?;
        Ok(self.reading.as_deref())
    }
    fn part_of_speech(&self) -> AnalysisResult<Option<&str>> {
        self.errors.check(0)?;
        Ok(self.part_of_speech.as_deref())
    }
    fn attributes(&self) -> [Option<&str>; 6] {
        self.fields().map(|field| field.map(String::as_str))
    }
}
impl JapaneseToken for AnalysisToken {
    fn reading(&self) -> AnalysisResult<Option<&str>> {
        let Some(value) = self.japanese_morphology() else {
            return Ok(None);
        };
        value.errors.check(2)?;
        Ok(value.reading.as_deref())
    }
    fn part_of_speech(&self) -> AnalysisResult<Option<&str>> {
        let Some(value) = self.japanese_morphology() else {
            return Ok(None);
        };
        value.errors.check(0)?;
        Ok(value.part_of_speech.as_deref())
    }
    fn attributes(&self) -> [Option<&str>; 6] {
        self.japanese_morphology().map_or([None; 6], |value| {
            value.fields().map(|field| field.map(String::as_str))
        })
    }
}
impl From<KuromojiOutput> for FilterStream<KuromojiToken> {
    fn from(output: KuromojiOutput) -> Self {
        Self {
            tokens: output.tokens,
            terminal: output.terminal,
            final_offset_utf16: output.final_offset_utf16,
            final_position_increment: output.final_position_increment,
            context: (),
        }
    }
}
impl From<FilterStream<KuromojiToken>> for KuromojiOutput {
    fn from(stream: FilterStream<KuromojiToken>) -> Self {
        Self {
            tokens: stream.tokens,
            terminal: stream.terminal,
            final_offset_utf16: stream.final_offset_utf16,
            final_position_increment: stream.final_position_increment,
        }
    }
}
impl FilterToken for KuromojiToken {
    type Context = ();
    fn term(&self) -> impl Iterator<Item = u16> + '_ {
        self.term_utf16.iter().copied()
    }
    fn term_len(&self, _work: &mut Work<'_>) -> AnalysisResult<usize> {
        Ok(self.term_utf16.len())
    }
    fn replace_term(
        &mut self,
        term: Budgeted<Vec<u16>>,
        memory: &mut MemoryReservation,
        (): &(),
        _work: &mut Work<'_>,
    ) -> AnalysisResult<()> {
        let bytes = self.term_utf16.capacity() * size_of::<u16>();
        let (term, allocation) = term.into_parts();
        drop(std::mem::replace(&mut self.term_utf16, term));
        drop(memory.split(bytes));
        memory.absorb(allocation);
        Ok(())
    }
    fn keyword(&self) -> bool {
        self.keyword
    }
}
