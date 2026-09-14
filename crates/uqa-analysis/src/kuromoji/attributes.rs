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
    pub origin: KuromojiOrigin,
}

impl JapaneseMorphology {
    fn fields(&self) -> [Option<&String>; 6] {
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
            },
            memory,
        ))
    }
}
