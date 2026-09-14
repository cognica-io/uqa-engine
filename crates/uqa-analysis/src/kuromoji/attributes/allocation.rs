//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Raw Japanese terms and independent morphology buffers retain exact reservation ownership.

use super::super::KuromojiToken;
use crate::allocation::{copy_text, copy_units};
use crate::token::allocation::AllocatedToken;
use crate::AnalysisResult;
use uqa_core::memory::MemoryError;
use uqa_core::memory::{Budgeted, MemoryBudget};

impl KuromojiToken {
    pub(crate) fn fields(&self) -> [Option<&String>; 6] {
        [
            self.part_of_speech.as_ref(),
            self.base_form.as_ref(),
            self.reading.as_ref(),
            self.pronunciation.as_ref(),
            self.inflection_type.as_ref(),
            self.inflection_form.as_ref(),
        ]
    }
    pub(crate) fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let (term_utf16, mut memory) = copy_units(&self.term_utf16, budget, poll)?.into_parts();
        let mut copies: [Option<String>; 6] = Default::default();
        for (field, target) in self.fields().into_iter().zip(&mut copies) {
            if let Some(field) = field {
                let (value, allocation) = copy_text(field, budget, poll)?.into_parts();
                *target = Some(value);
                memory.absorb(allocation);
            }
        }
        let [part_of_speech, base_form, reading, pronunciation, inflection_type, inflection_form] =
            copies;
        Ok(Budgeted::new(
            Self {
                term_utf16,
                part_of_speech,
                base_form,
                reading,
                pronunciation,
                inflection_type,
                inflection_form,
                start_utf16: self.start_utf16,
                end_utf16: self.end_utf16,
                position_increment: self.position_increment,
                position_length: self.position_length,
                keyword: self.keyword,
                origin: self.origin,
                errors: self.errors,
            },
            memory,
        ))
    }
}
impl AllocatedToken for KuromojiToken {
    fn allocation_bytes(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        poll()?;
        let mut bytes = self.term_utf16.capacity() * size_of::<u16>();
        for field in self.fields().into_iter().flatten() {
            bytes = bytes
                .checked_add(field.capacity())
                .ok_or(MemoryError::SizeOverflow)?;
        }
        Ok(bytes)
    }
    fn increment(&self) -> u32 {
        self.position_increment
    }
    fn set_increment(&mut self, increment: u32) {
        self.position_increment = increment;
    }
}
