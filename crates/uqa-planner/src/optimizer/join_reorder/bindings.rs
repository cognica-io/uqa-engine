//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assign predicates to contiguous ranges in the winning tree's postorder, preserving their input order at each join.

use crate::JoinPlan;
use std::ops::Range;
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation, ProductionControl};
use uqa_sql::{plan::OwnedScalarExpr, SQLError};

pub(super) struct PredicateGroups {
    offsets: [usize; 64],
    nodes: usize,
}

impl PredicateGroups {
    /// Reuse the search masks as a permutation from input order to final predicate slots.
    pub(super) fn assign(
        tree: &JoinPlan,
        masks: &mut [u64],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let nodes = tree.relations.count_ones() as usize - 1;
        let mut offsets = [0; 64];
        for mask in &mut *masks {
            control.check()?;
            let node = destination(tree, *mask, nodes - 1);
            offsets[node + 1] += 1;
            *mask = node as u64;
        }
        for node in 0..nodes {
            offsets[node + 1] += offsets[node];
        }
        let mut next = offsets;
        for slot in masks {
            control.check()?;
            let next_slot = &mut next[usize::try_from(*slot).expect("join node fits in usize")];
            *slot = *next_slot as u64;
            *next_slot += 1;
        }
        Ok(Self { offsets, nodes })
    }

    pub(super) fn ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.offsets[..=self.nodes]
            .windows(2)
            .map(|pair| pair[0]..pair[1])
    }
}

// Each binary subtree with n leaves has n - 1 join nodes. A residual with no known relation belongs to the root.
fn destination(tree: &JoinPlan, relations: u64, node: usize) -> usize {
    if relations != 0 {
        let left = tree
            .left
            .as_deref()
            .expect("selected join has a left child");
        let right = tree
            .right
            .as_deref()
            .expect("selected join has a right child");
        if left.left.is_some() && relations & !left.relations == 0 {
            return destination(
                left,
                relations,
                node - right.relations.count_ones() as usize,
            );
        }
        if right.left.is_some() && relations & !right.relations == 0 {
            return destination(right, relations, node - 1);
        }
    }
    node
}

// Free the slots and any unconsumed expressions before releasing the temporary buffer's reservation.
pub(super) struct PredicateBindings {
    values: Vec<Option<OwnedScalarExpr>>,
    _memory: Option<MemoryReservation>,
}

impl PredicateBindings {
    pub(super) fn new(count: usize, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        control.check()?;
        let (mut values, memory) = if let Some(budget) = control.budget() {
            let mut values = BudgetedVec::new(budget);
            values.reserve(count)?;
            let (values, memory) = values.into_parts();
            (values, Some(memory))
        } else {
            let mut values = Vec::new();
            values.try_reserve_exact(count).map_err(MemoryError::from)?;
            (values, None)
        };
        values.resize_with(count, || None);
        Ok(Self {
            values,
            _memory: memory,
        })
    }

    pub(super) fn set(&mut self, index: u64, value: OwnedScalarExpr) {
        self.values[usize::try_from(index).expect("slot was derived from a slice index")] =
            Some(value);
    }

    pub(super) fn take(
        &mut self,
        range: Range<usize>,
    ) -> impl ExactSizeIterator<Item = OwnedScalarExpr> + '_ {
        self.values[range]
            .iter_mut()
            .map(|slot| slot.take().expect("assigned join predicate"))
    }
}
