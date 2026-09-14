//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Morphology copies keep every reading and morpheme allocation reserved until destruction.

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError, MemoryReservation};

use super::{KoreanMorphology, NoriMorpheme};
use crate::allocation::{copy_text, copy_units};
use crate::nori::NoriToken;
use crate::token::allocation::AllocatedToken;
use crate::AnalysisResult;

struct MorphemeBuffer {
    values: BudgetedVec<NoriMorpheme>,
    memory: MemoryReservation,
}

fn copy_morphemes(
    input: &[NoriMorpheme],
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<NoriMorpheme>>> {
    poll()?;
    let mut output = MorphemeBuffer {
        values: BudgetedVec::new(budget),
        memory: budget.empty_reservation(),
    };
    output.values.reserve(input.len())?;
    for morpheme in input {
        let (surface_utf16, memory) =
            copy_units(&morpheme.surface_utf16, budget, poll)?.into_parts();
        output.memory.absorb(memory);
        output.values.push(NoriMorpheme {
            surface_utf16,
            pos: morpheme.pos,
        })?;
    }
    let (values, mut memory) = output.values.into_parts();
    memory.absorb(output.memory);
    Ok(Budgeted::new(values, memory))
}

fn morphology_bytes(
    reading: Option<&String>,
    morphemes: Option<&Vec<NoriMorpheme>>,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<usize> {
    poll()?;
    let mut bytes = reading.map_or(0, String::capacity);
    if let Some(morphemes) = morphemes {
        bytes = bytes
            .checked_add(morphemes.capacity() * size_of::<NoriMorpheme>())
            .ok_or(MemoryError::SizeOverflow)?;
        for (index, morpheme) in morphemes.iter().enumerate() {
            if index % 1024 == 0 {
                poll()?;
            }
            bytes = bytes
                .checked_add(morpheme.surface_utf16.capacity() * size_of::<u16>())
                .ok_or(MemoryError::SizeOverflow)?;
        }
    }
    Ok(bytes)
}

type CopiedMorphology = (Option<String>, Option<Vec<NoriMorpheme>>);

fn copy_morphology(
    reading: Option<&str>,
    morphemes: Option<&[NoriMorpheme]>,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<CopiedMorphology>> {
    poll()?;
    let reading = reading
        .map(|text| copy_text(text, budget, poll))
        .transpose()?;
    let morphemes = morphemes
        .map(|parts| copy_morphemes(parts, budget, poll))
        .transpose()?;
    let mut memory = budget.empty_reservation();
    let reading = reading.map(|value| {
        let (value, allocation) = value.into_parts();
        memory.absorb(allocation);
        value
    });
    let morphemes = morphemes.map(|value| {
        let (value, allocation) = value.into_parts();
        memory.absorb(allocation);
        value
    });
    Ok(Budgeted::new((reading, morphemes), memory))
}

impl KoreanMorphology {
    pub(crate) fn allocation_bytes(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        morphology_bytes(self.reading.as_ref(), self.morphemes.as_ref(), poll)
    }

    pub(crate) fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let ((reading, morphemes), memory) = copy_morphology(
            self.reading.as_deref(),
            self.morphemes.as_deref(),
            budget,
            poll,
        )?
        .into_parts();
        Ok(Budgeted::new(
            Self {
                pos_type: self.pos_type,
                left_pos: self.left_pos,
                right_pos: self.right_pos,
                reading,
                morphemes,
                origin: self.origin,
            },
            memory,
        ))
    }
}

impl AllocatedToken for NoriToken {
    fn allocation_bytes(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        Ok(
            morphology_bytes(self.reading.as_ref(), self.morphemes.as_ref(), poll)?
                .checked_add(self.term_utf16.capacity() * size_of::<u16>())
                .ok_or(MemoryError::SizeOverflow)?,
        )
    }
    fn increment(&self) -> u32 {
        self.position_increment
    }
    fn set_increment(&mut self, increment: u32) {
        self.position_increment = increment;
    }
}

impl NoriToken {
    pub(crate) fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        let term = copy_units(&self.term_utf16, budget, poll)?;
        let morphology = copy_morphology(
            self.reading.as_deref(),
            self.morphemes.as_deref(),
            budget,
            poll,
        )?;
        let (term_utf16, mut memory) = term.into_parts();
        let ((reading, morphemes), allocation) = morphology.into_parts();
        memory.absorb(allocation);
        Ok(Budgeted::new(
            Self {
                term_utf16,
                reading,
                morphemes,
                start_utf16: self.start_utf16,
                end_utf16: self.end_utf16,
                position_increment: self.position_increment,
                position_length: self.position_length,
                keyword: self.keyword,
                pos_type: self.pos_type,
                left_pos: self.left_pos,
                right_pos: self.right_pos,
                origin: self.origin,
            },
            memory,
        ))
    }
}
