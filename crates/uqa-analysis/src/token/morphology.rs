//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A token has at most one language's attributes while preserving public diagnostic keys.

use serde::Serialize;
use uqa_core::memory::{Budgeted, MemoryBudget};

use crate::AnalysisResult;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) enum Morphology {
    #[cfg(feature = "nori")]
    #[serde(rename = "korean_morphology")]
    Korean(crate::nori::KoreanMorphology),
    #[cfg(feature = "kuromoji")]
    #[serde(rename = "japanese_morphology")]
    Japanese(crate::kuromoji::JapaneseMorphology),
}

impl Morphology {
    pub(super) fn allocation_bytes(
        &self,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<usize> {
        match self {
            #[cfg(feature = "nori")]
            Self::Korean(value) => value.allocation_bytes(poll),
            #[cfg(feature = "kuromoji")]
            Self::Japanese(value) => value.allocation_bytes(poll),
        }
    }

    pub(super) fn clone_budgeted(
        &self,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        match self {
            #[cfg(feature = "nori")]
            Self::Korean(value) => {
                let (value, memory) = value.clone_budgeted(budget, poll)?.into_parts();
                Ok(Budgeted::new(Self::Korean(value), memory))
            }
            #[cfg(feature = "kuromoji")]
            Self::Japanese(value) => {
                let (value, memory) = value.clone_budgeted(budget, poll)?.into_parts();
                Ok(Budgeted::new(Self::Japanese(value), memory))
            }
        }
    }
}
