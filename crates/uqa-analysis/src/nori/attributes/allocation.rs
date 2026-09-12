//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Morphology copies keep every reading and morpheme allocation reserved until destruction.

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

use super::{KoreanMorphology, NoriMorpheme};
use crate::allocation::{copy_text, copy_units};
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

impl KoreanMorphology {
    pub(crate) fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        poll()?;
        let reading = self
            .reading
            .as_deref()
            .map(|text| copy_text(text, budget, poll))
            .transpose()?;
        let morphemes = self
            .morphemes
            .as_deref()
            .map(|values| copy_morphemes(values, budget, poll))
            .transpose()?;
        let mut memory = budget.empty_reservation();
        let reading = reading.map(|reading| {
            let (value, allocation) = reading.into_parts();
            memory.absorb(allocation);
            value
        });
        let morphemes = morphemes.map(|morphemes| {
            let (value, allocation) = morphemes.into_parts();
            memory.absorb(allocation);
            value
        });
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
