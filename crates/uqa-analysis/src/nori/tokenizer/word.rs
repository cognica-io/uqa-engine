//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A common view of system, unknown, and user morphology without narrowing user costs.

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

use super::allocation;
use super::lattice::WordId;
use super::{NoriMorpheme, NoriOrigin};
use crate::nori::{DictionaryWord, NoriDictionary, POSTag, POSType, UserDictionary, UserEntry};
use crate::AnalysisResult;

pub(super) enum Word<'a> {
    Dictionary(DictionaryWord<'a>, NoriOrigin),
    User(&'a UserEntry),
}

impl<'a> Word<'a> {
    pub fn resolve(
        id: WordId,
        model: &'a NoriDictionary,
        user: Option<&'a UserDictionary>,
    ) -> Self {
        match id {
            WordId::Known(id) => Self::Dictionary(
                model.word(id).expect("validated known word"),
                NoriOrigin::Known,
            ),
            WordId::Unknown(id) => Self::Dictionary(
                model.word(id).expect("validated unknown word"),
                NoriOrigin::Unknown,
            ),
            WordId::User(id) => Self::User(
                user.expect("user candidate has a model")
                    .entry(id)
                    .expect("validated user word"),
            ),
        }
    }
    pub fn cost(&self) -> i32 {
        match self {
            Self::Dictionary(word, _) => i32::from(word.cost()),
            Self::User(word) => word.cost(),
        }
    }
    pub fn left(&self) -> u16 {
        match self {
            Self::Dictionary(word, _) => word.left_context(),
            Self::User(word) => word.left_context(),
        }
    }
    pub fn right(&self) -> u16 {
        match self {
            Self::Dictionary(word, _) => word.right_context(),
            Self::User(word) => word.right_context(),
        }
    }
    pub fn pos_type(&self) -> POSType {
        match self {
            Self::Dictionary(word, _) => word.pos_type(),
            Self::User(word) => word.pos_type(),
        }
    }
    pub fn left_pos(&self) -> POSTag {
        match self {
            Self::Dictionary(word, _) => word.left_pos(),
            Self::User(word) => word.pos(),
        }
    }
    pub fn right_pos(&self) -> POSTag {
        match self {
            Self::Dictionary(word, _) => word.right_pos(),
            Self::User(word) => word.pos(),
        }
    }
    pub fn origin(&self) -> NoriOrigin {
        match self {
            Self::Dictionary(_, origin) => *origin,
            Self::User(_) => NoriOrigin::User,
        }
    }
    pub fn reading(&self) -> Option<&str> {
        match self {
            Self::Dictionary(word, _) => word.reading(),
            Self::User(_) => None,
        }
    }
    pub fn morphemes(
        &self,
        surface: &[u16],
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Option<Vec<NoriMorpheme>>>> {
        let mut attributes = budget.empty_reservation();
        let mut result = BudgetedVec::new(budget);
        match self {
            Self::Dictionary(word, _) => {
                let Some(parts) = word.morphemes() else {
                    return Ok(Budgeted::new(None, attributes));
                };
                poll()?;
                result.reserve(parts.len())?;
                for part in parts {
                    let (surface_utf16, memory) =
                        allocation::encode(part.surface, usize::MAX, budget, poll)?.into_parts();
                    attributes.absorb(memory);
                    result.push(NoriMorpheme {
                        surface_utf16,
                        pos: part.pos,
                    })?;
                }
            }
            Self::User(word) => {
                let Some(lengths) = word.segment_lengths() else {
                    return Ok(Budgeted::new(None, attributes));
                };
                poll()?;
                result.reserve(lengths.len())?;
                let mut offset = 0;
                for length in lengths {
                    let (surface_utf16, memory) =
                        allocation::copy_units(&surface[offset..offset + length], budget, poll)?
                            .into_parts();
                    attributes.absorb(memory);
                    result.push(NoriMorpheme {
                        surface_utf16,
                        pos: word.pos(),
                    })?;
                    offset += length;
                }
            }
        }
        let (result, memory) = result.into_parts();
        attributes.absorb(memory);
        Ok(Budgeted::new(Some(result), attributes))
    }
}
