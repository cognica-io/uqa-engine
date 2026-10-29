//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Matching candidates own their admitted container throughout fallible ranking.

use crate::SQLError;
use uqa_core::memory::{BudgetedVec, MemoryReservation, ProductionControl};

pub(crate) enum Candidates<T> {
    Ordinary(Vec<T>),
    Controlled(BudgetedVec<T>),
}

impl<T> Candidates<T> {
    pub(crate) fn new(control: &ProductionControl<'_>) -> Self {
        control.budget().map_or_else(
            || Self::Ordinary(Vec::new()),
            |budget| Self::Controlled(BudgetedVec::new(budget)),
        )
    }

    pub(crate) fn push(&mut self, candidate: T) -> Result<(), SQLError> {
        match self {
            Self::Ordinary(values) => values.push(candidate),
            Self::Controlled(values) => values.push(candidate)?,
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> RankingCandidates<T> {
        let (values, memory) = match self {
            Self::Ordinary(values) => (values, None),
            Self::Controlled(values) => {
                let (values, memory) = values.into_parts();
                (values, Some(memory))
            }
        };
        RankingCandidates {
            values,
            _memory: memory,
        }
    }
}

pub(crate) struct RankingCandidates<T> {
    pub(crate) values: Vec<T>,
    _memory: Option<MemoryReservation>,
}
