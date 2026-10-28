//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate slots admit their temporary buffer; expression payloads retain their separate source or output owners.

use super::{JoinPredicateBinding, ScalarExpr, SourcePlan};
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation, ProductionControl};
use uqa_sql::SQLError;

pub(super) struct PredicateBindings {
    values: Vec<JoinPredicateBinding>,
    // Field order releases the buffer and any remaining expression before its slot reservation.
    _memory: Option<MemoryReservation>,
}

impl PredicateBindings {
    pub(super) fn new(count: usize, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        control.check()?;
        let (values, memory) = if let Some(budget) = control.budget() {
            let mut values = BudgetedVec::new(budget);
            values.reserve(count)?;
            let (values, memory) = values.into_parts();
            (values, Some(memory))
        } else {
            let mut values = Vec::new();
            values.try_reserve_exact(count).map_err(MemoryError::from)?;
            (values, None)
        };
        Ok(Self {
            values,
            _memory: memory,
        })
    }

    pub(super) fn push(&mut self, binding: JoinPredicateBinding) {
        assert!(
            self.values.len() < self.values.capacity(),
            "predicate slots were admitted before insertion"
        );
        self.values.push(binding);
    }

    pub(super) fn as_slice(&self) -> &[JoinPredicateBinding] {
        &self.values
    }

    pub(super) fn as_mut_slice(&mut self) -> &mut [JoinPredicateBinding] {
        &mut self.values
    }
}

pub(super) fn attach_join_predicates(
    source: &mut SourcePlan,
    selected: &crate::JoinPlan,
    predicates: &mut [JoinPredicateBinding],
    root: bool,
) {
    let SourcePlan::Join {
        left, right, on, ..
    } = source
    else {
        return;
    };
    attach_join_predicates(
        left,
        selected.left.as_deref().expect("selected join left"),
        predicates,
        false,
    );
    attach_join_predicates(
        right,
        selected.right.as_deref().expect("selected join right"),
        predicates,
        false,
    );
    let assign = |predicate: &JoinPredicateBinding| {
        predicate.expression.is_some()
            && (root
                || (predicate.relations != 0 && predicate.relations & !selected.relations == 0))
    };
    let count = predicates
        .iter()
        .filter(|predicate| assign(predicate))
        .count();
    *on = match count {
        0 => None,
        1 => predicates
            .iter_mut()
            .find(|predicate| assign(predicate))
            .and_then(|predicate| predicate.expression.take()),
        _ => {
            let mut assigned = Vec::with_capacity(count);
            for predicate in predicates {
                if assign(predicate) {
                    assigned.push(predicate.expression.take().expect("assigned predicate"));
                }
            }
            Some(ScalarExpr::And(assigned))
        }
    };
}
