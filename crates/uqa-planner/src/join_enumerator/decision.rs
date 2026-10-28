//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copy-only subset decisions and their exact node admission.

use super::{JoinEdge, JoinPlan};
use crate::cost_model::OperatorKind;
use uqa_core::{
    memory::{MemoryReservation, OwnedMap, Produced, ProductionControl},
    ValueRetentionError,
};

#[derive(Clone, Copy, Debug)]
pub(super) struct Decision {
    pub relations: u64,
    pub cardinality: f64,
    pub cost: f64,
    pub left: u64,
    pub right: u64,
    pub edge: Option<JoinEdge>,
    pub kind: Option<OperatorKind>,
}

impl Decision {
    pub const EMPTY: Self = Self {
        relations: 0,
        cardinality: 0.0,
        cost: 0.0,
        left: 0,
        right: 0,
        edge: None,
        kind: None,
    };

    pub fn leaf(index: usize, rows: f64, access_cost: f64) -> Self {
        Self {
            relations: 1_u64 << index,
            cardinality: rows,
            cost: access_cost,
            ..Self::EMPTY
        }
    }
}

pub(super) struct Memo<'a, T: Copy> {
    values: OwnedMap<u64, T>,
    // Drop actual nodes before releasing their combined reservation.
    memory: Option<MemoryReservation>,
    control: ProductionControl<'a>,
}

impl<'a, T: Copy> Memo<'a, T> {
    pub fn new(control: ProductionControl<'a>) -> Self {
        Self {
            values: OwnedMap::new(),
            memory: control.empty_reservation(),
            control,
        }
    }

    pub fn get(&self, mask: u64) -> Option<T> {
        self.values.get(&mask).copied()
    }

    pub fn insert(&mut self, mask: u64, value: T) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        if self.values.contains_key(&mask) {
            self.values.insert(mask, value);
            return Ok(());
        }
        let memory = self.control.reserve(OwnedMap::<u64, T>::entry_bytes())?;
        self.memory = self.control.combine(self.memory.take(), memory);
        self.values.insert(mask, value);
        Ok(())
    }
}

impl Memo<'_, Decision> {
    pub fn produce(&self, mask: u64) -> Result<Produced<JoinPlan>, ValueRetentionError> {
        // A binary tree over n distinct leaves has exactly 2(n - 1) boxed
        // children. The root is returned by value; neither it nor a leaf has
        // another separately allocated payload.
        let children = 2 * (mask.count_ones() as usize - 1);
        let memory = self.control.reserve(children * size_of::<JoinPlan>())?;
        let plan = self.expand(mask)?;
        self.control.finish(plan, memory)
    }

    fn expand(&self, mask: u64) -> Result<JoinPlan, ValueRetentionError> {
        self.control.check()?;
        let decision = self
            .get(mask)
            .expect("selected subset has a settled decision");
        let (left, right) = if decision.left == 0 {
            (None, None)
        } else {
            debug_assert_eq!(decision.left & decision.right, 0);
            debug_assert_eq!(decision.left | decision.right, mask);
            (
                Some(Box::new(self.expand(decision.left)?)),
                Some(Box::new(self.expand(decision.right)?)),
            )
        };
        Ok(JoinPlan {
            relations: mask,
            cardinality: decision.cardinality,
            cost: decision.cost,
            left,
            right,
            join_edge: decision.edge,
            kind: decision.kind,
        })
    }
}
