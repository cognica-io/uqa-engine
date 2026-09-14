//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese attributes retain absent values independently of their token representation.

use serde::Serialize;
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError};

use super::KuromojiOrigin;
use crate::{allocation::copy_text, AnalysisResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JapaneseMorphology {
    pub part_of_speech: Option<String>,
    pub base_form: Option<String>,
    pub reading: Option<String>,
    pub pronunciation: Option<String>,
    pub inflection_type: Option<String>,
    pub inflection_form: Option<String>,
    /// Absent on generated tokens with no dictionary provenance.
    pub origin: Option<KuromojiOrigin>,
    #[serde(skip)]
    pub(crate) errors: AttributeErrors,
}

mod allocation;

impl JapaneseMorphology {
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

    pub(crate) fn allocation_bytes(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        poll()?;
        let mut bytes = 0_usize;
        for value in self.fields().into_iter().flatten() {
            bytes = bytes
                .checked_add(value.capacity())
                .ok_or(MemoryError::SizeOverflow)?;
        }
        Ok(bytes)
    }

    pub(crate) fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        poll()?;
        let mut copies: [Option<String>; 6] = Default::default();
        let mut memory = budget.empty_reservation();
        for (source, target) in self.fields().into_iter().zip(&mut copies) {
            if let Some(source) = source {
                let (value, allocation) = copy_text(source, budget, poll)?.into_parts();
                memory.absorb(allocation);
                *target = Some(value);
            }
        }
        let [part_of_speech, base_form, reading, pronunciation, inflection_type, inflection_form] =
            copies;
        Ok(Budgeted::new(
            Self {
                part_of_speech,
                base_form,
                reading,
                pronunciation,
                inflection_type,
                inflection_form,
                origin: self.origin,
                errors: self.errors,
            },
            memory,
        ))
    }
}

/// Deferred Java user-field access failures remain private to unobserved stream attributes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AttributeErrors(u8);
impl AttributeErrors {
    pub(crate) fn record(&mut self, index: usize) {
        self.0 |= 1 << index;
    }
    pub(crate) fn check(self, index: usize) -> crate::AnalysisResult<()> {
        if self.0 & (1 << index) != 0 {
            return Err(super::error::invalid(
                "user dictionary",
                "requested morphology field is absent",
            )
            .into());
        }
        Ok(())
    }
    pub(crate) fn validate(self) -> crate::AnalysisResult<()> {
        if self.0 != 0 {
            return Err(super::error::invalid(
                "user dictionary",
                "requested morphology field is absent",
            )
            .into());
        }
        Ok(())
    }
}
