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

/// Destination masks and conjunction buffers are complete before any source node moves.
pub(super) struct PredicateAttachments {
    buffers: [Option<super::output::Conjunction>; 63],
    count: usize,
}

impl PredicateAttachments {
    pub(super) fn prepare(
        selected: &crate::JoinPlan,
        predicates: &mut [JoinPredicateBinding],
        output: &super::Output,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        for predicate in predicates.iter_mut() {
            predicate.relations = destination(selected, predicate.relations);
        }
        let mut prepared = Self {
            buffers: std::array::from_fn(|_| None),
            count: 0,
        };
        prepared.prepare_nodes(selected, predicates, output, control)?;
        Ok(prepared)
    }

    fn prepare_nodes(
        &mut self,
        tree: &crate::JoinPlan,
        predicates: &[JoinPredicateBinding],
        output: &super::Output,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let (Some(left), Some(right)) = (&tree.left, &tree.right) else {
            return Ok(());
        };
        self.prepare_nodes(left, predicates, output, control)?;
        self.prepare_nodes(right, predicates, output, control)?;
        let count = predicates
            .iter()
            .filter(|predicate| predicate.relations == tree.relations)
            .count();
        if count > 1 {
            self.buffers[self.count] = Some(output.prepare_conjunction(count, control)?);
        }
        self.count += 1;
        Ok(())
    }

    pub(super) fn attach(
        mut self,
        source: &mut SourcePlan,
        selected: &crate::JoinPlan,
        predicates: &mut [JoinPredicateBinding],
        output: &mut super::Output,
    ) {
        let mut index = 0;
        self.attach_nodes(source, selected, predicates, output, &mut index);
        assert_eq!(
            index, self.count,
            "each selected join receives its prepared output"
        );
    }

    fn attach_nodes(
        &mut self,
        source: &mut SourcePlan,
        selected: &crate::JoinPlan,
        predicates: &mut [JoinPredicateBinding],
        output: &mut super::Output,
        index: &mut usize,
    ) {
        let SourcePlan::Join {
            left, right, on, ..
        } = source
        else {
            return;
        };
        self.attach_nodes(
            left,
            selected.left.as_deref().expect("selected join left"),
            predicates,
            output,
            index,
        );
        self.attach_nodes(
            right,
            selected.right.as_deref().expect("selected join right"),
            predicates,
            output,
            index,
        );
        let mut conjunction = self.buffers[*index].take();
        *index += 1;
        for predicate in predicates
            .iter_mut()
            .filter(|predicate| predicate.relations == selected.relations)
        {
            let expression = predicate.expression.take().expect("assigned predicate");
            if let Some(conjunction) = &mut conjunction {
                conjunction.push(expression);
            } else {
                assert!(
                    on.is_none(),
                    "multiple predicates have an admitted conjunction"
                );
                *on = Some(expression);
            }
        }
        if let Some(conjunction) = conjunction {
            *on = Some(conjunction.publish(output));
        }
    }
}

pub(super) fn destination(tree: &crate::JoinPlan, relations: u64) -> u64 {
    if relations != 0 {
        for child in [tree.left.as_deref(), tree.right.as_deref()]
            .into_iter()
            .flatten()
        {
            if child.left.is_some() && relations & !child.relations == 0 {
                return destination(child, relations);
            }
        }
    }
    tree.relations
}
