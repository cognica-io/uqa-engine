//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move each source, original predicate and reusable join shell exactly once into the selected topology.

use super::{MutableSourcePlan, OwnedScalarExpr, SQLError};
use crate::{JoinExecutionStrategy, JoinPlan, OperatorKind};
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation, ProductionControl};
use uqa_sql::plan::{MutableJoinShell, SourcePlan};

pub(super) struct Predicates {
    values: Vec<Option<OwnedScalarExpr>>,
    memory: Option<MemoryReservation>,
}
impl Predicates {
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
        Ok(Self { values, memory })
    }
    pub(super) fn set(&mut self, index: usize, value: OwnedScalarExpr) {
        self.values[index] = Some(value);
    }
    fn take(&mut self, index: usize) -> OwnedScalarExpr {
        self.values[index]
            .take()
            .expect("selected predicate ownership")
    }
    pub(super) fn all_consumed(&self) -> bool {
        self.values.iter().all(Option::is_none)
    }
}

pub(super) fn reorder(
    source: MutableSourcePlan,
    tree: &JoinPlan,
    masks: &[u64],
    predicates: &mut Predicates,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    let mut sources = Sources {
        atoms: std::array::from_fn(|_| None),
        shells: std::array::from_fn(|_| None),
        atom_count: 0,
        shell_count: 0,
        predicate_count: 0,
    };
    sources.collect(source, predicates, control)?;
    sources.materialize(tree, masks, predicates, control)
}
struct Sources {
    atoms: [Option<MutableSourcePlan>; 64],
    shells: [Option<MutableJoinShell>; 63],
    atom_count: usize,
    shell_count: usize,
    predicate_count: usize,
}
impl Sources {
    fn collect(
        &mut self,
        source: MutableSourcePlan,
        predicates: &mut Predicates,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if !matches!(&*source, SourcePlan::Join { .. }) {
            self.atoms[self.atom_count] = Some(source);
            self.atom_count += 1;
            return Ok(());
        }
        let parts = source.split_join(control)?;
        self.collect(parts.left, predicates, control)?;
        self.collect(parts.right, predicates, control)?;
        if let Some(on) = parts.on {
            self.conjuncts(on, predicates, control)?;
        }
        self.shells[self.shell_count] = Some(parts.shell);
        self.shell_count += 1;
        Ok(())
    }
    fn conjuncts(
        &mut self,
        expression: OwnedScalarExpr,
        predicates: &mut Predicates,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if matches!(&*expression, uqa_sql::ScalarExpr::And(_)) {
            for child in expression.into_children() {
                self.conjuncts(child, predicates, control)?;
            }
        } else {
            predicates.set(self.predicate_count, expression);
            self.predicate_count += 1;
        }
        Ok(())
    }
    fn materialize(
        &mut self,
        tree: &JoinPlan,
        masks: &[u64],
        predicates: &mut Predicates,
        control: &ProductionControl<'_>,
    ) -> Result<MutableSourcePlan, SQLError> {
        control.check()?;
        let (Some(left), Some(right)) = (&tree.left, &tree.right) else {
            assert!(
                tree.left.is_none() && tree.right.is_none(),
                "selected join children are paired"
            );
            let index = tree.relations.trailing_zeros() as usize;
            return Ok(self.atoms[index].take().expect("selected source atom"));
        };
        let left = self.materialize(left, masks, predicates, control)?;
        let right = self.materialize(right, masks, predicates, control)?;
        let strategy = match (tree.join_edge, tree.kind) {
            (None, _) => JoinExecutionStrategy::Auto,
            (Some(_), Some(OperatorKind::HashJoinInner)) => JoinExecutionStrategy::Hash,
            _ => {
                return Err(SQLError::Internal(
                    "DPccp selected an invalid executable equijoin strategy".into(),
                ))
            }
        };
        let count = masks.iter().filter(|mask| **mask == tree.relations).count();
        let on = match count {
            0 => None,
            1 => Some(
                predicates.take(
                    masks
                        .iter()
                        .position(|mask| *mask == tree.relations)
                        .expect("selected predicate"),
                ),
            ),
            _ => {
                // The original predicate order is retained. The scalar owner admits its exact conjunction destination before it consumes these children.
                let mut selected = Predicates::new(count, control)?;
                let mut position = 0;
                for (index, mask) in masks.iter().enumerate() {
                    if *mask == tree.relations {
                        selected.set(position, predicates.take(index));
                        position += 1;
                    }
                }
                let (values, memory) = (selected.values, selected.memory);
                let output = OwnedScalarExpr::boolean(
                    values
                        .into_iter()
                        .map(|value| value.expect("selected conjunct")),
                    true,
                    control,
                )?;
                drop(memory);
                Some(output)
            }
        };
        self.shell_count -= 1;
        self.shells[self.shell_count]
            .take()
            .expect("selected join shell")
            .assemble(left, right, on, strategy, control)
    }
}
