//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use super::invalid;
use crate::diskann_index::SquaredNavigationDistance;
use crate::{read_control::StorageReadControl, vector_index::DiskANNAlpha, StorageBackendResult};

/// `RobustPrune` over unique, non-self candidates in source-distance/node order. Providers retain coordinates; this cursor retains only the selected IDs.
pub(in crate::diskann_index) struct Selection {
    alpha: DiskANNAlpha,
    degree: usize,
    selected: BudgetedVec<u64>,
}

impl Selection {
    pub(in crate::diskann_index) fn new(
        alpha: DiskANNAlpha,
        degree: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut selected = BudgetedVec::new(control.memory());
        selected.reserve(degree)?;
        Ok(Self {
            alpha,
            degree,
            selected,
        })
    }

    pub(in crate::diskann_index) fn is_full(&self) -> bool {
        self.selected.len() == self.degree
    }

    /// Return whether the candidate was admitted. The callback receives each selected neighbor's stable insertion position and ID.
    pub(in crate::diskann_index) fn consider(
        &mut self,
        candidate: u64,
        distance: f64,
        control: &StorageReadControl,
        mut between: impl FnMut(usize, u64) -> StorageBackendResult<SquaredNavigationDistance>,
    ) -> StorageBackendResult<bool> {
        control.check()?;
        if !distance.is_finite() || distance.is_sign_negative() {
            return Err(invalid("invalid source navigation distance"));
        }
        if self.is_full() {
            return Ok(false);
        }
        for (position, &neighbor) in self.selected.iter().enumerate() {
            control.check()?;
            if self.alpha.squared() * between(position, neighbor)?.get() <= distance {
                return Ok(false);
            }
        }
        self.selected.push(candidate)?;
        Ok(true)
    }

    pub(in crate::diskann_index) fn finish(self) -> BudgetedVec<u64> {
        self.selected
    }
}
