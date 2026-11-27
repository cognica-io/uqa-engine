//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move sources, predicates and their original join shells into the selected topology.

use super::{
    bindings::{PredicateBindings, PredicateGroups},
    selection::SelectedGraph,
};
use crate::{JoinExecutionStrategy, JoinPlan, OperatorKind};
use std::ops::Range;
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl};
use uqa_sql::{
    plan::{MutableJoinShell, MutableSourcePlan, OwnedScalarExpr, SourcePlan},
    SQLError, ScalarExpr,
};

/// Embedded plans carry their constructor reservations. Caller-owned plans retain newly constructed predicates in their output owner.
pub(super) trait PredicateOutput {
    fn copy_inferred(
        &mut self,
        expression: &ScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError>;

    fn prepare(
        &mut self,
        _groups: &PredicateGroups,
        _control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        Ok(())
    }

    fn take_conjunction_memory(&mut self, _items: &[ScalarExpr]) -> Option<MemoryReservation> {
        None
    }

    fn conjunction(
        &mut self,
        node: usize,
        children: impl ExactSizeIterator<Item = OwnedScalarExpr>,
        storage: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError>;
}

pub(super) struct EmbeddedPredicates;

impl PredicateOutput for EmbeddedPredicates {
    fn copy_inferred(
        &mut self,
        expression: &ScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        Ok(expression.copy_for_rewrite(control)?)
    }

    fn conjunction(
        &mut self,
        _node: usize,
        children: impl ExactSizeIterator<Item = OwnedScalarExpr>,
        storage: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        Ok(OwnedScalarExpr::boolean(children, true, storage)?)
    }
}

pub(super) struct PreparedJoin {
    tree: Produced<JoinPlan>,
    positions: Produced<Vec<u64>>,
    original_count: usize,
    groups: PredicateGroups,
    predicates: PredicateBindings,
}

impl PreparedJoin {
    pub(super) fn new(
        mut selected: SelectedGraph<'_>,
        output: &mut impl PredicateOutput,
        work: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let groups = PredicateGroups::assign(&selected.tree, selected.masks.as_mut_slice(), work)?;
        let mut predicates = PredicateBindings::new(selected.masks.len(), work)?;
        for (&slot, &expression) in selected.masks[selected.original_count..]
            .iter()
            .zip(selected.inferred.iter())
        {
            work.check()?;
            predicates.set(slot, output.copy_inferred(expression, work)?);
        }
        output.prepare(&groups, work)?;
        Ok(Self {
            tree: selected.tree,
            positions: selected.masks,
            original_count: selected.original_count,
            groups,
            predicates,
        })
    }

    pub(super) fn assemble(
        mut self,
        source: MutableSourcePlan,
        output: &mut impl PredicateOutput,
        storage: &ProductionControl<'_>,
    ) -> Result<MutableSourcePlan, SQLError> {
        let mut sources = Sources {
            atoms: std::array::from_fn(|_| None),
            shells: std::array::from_fn(|_| None),
            atom_count: 0,
            shell_count: 0,
        };
        let mut originals = self.positions[..self.original_count].iter().copied();
        sources.collect(
            source,
            &mut self.predicates,
            &mut originals,
            output,
            storage,
        )?;
        assert!(
            originals.next().is_none(),
            "all original join predicates moved"
        );
        drop(self.positions);
        let mut nodes = self.groups.ranges().enumerate();
        let plan = sources.materialize(
            &self.tree,
            &mut nodes,
            &mut self.predicates,
            output,
            storage,
        )?;
        assert!(
            nodes.next().is_none(),
            "all join nodes received their predicates"
        );
        Ok(plan)
    }
}

struct Sources {
    atoms: [Option<MutableSourcePlan>; 64],
    shells: [Option<MutableJoinShell>; 63],
    atom_count: usize,
    shell_count: usize,
}

impl Sources {
    fn collect(
        &mut self,
        source: MutableSourcePlan,
        predicates: &mut PredicateBindings,
        originals: &mut impl Iterator<Item = u64>,
        output: &mut impl PredicateOutput,
        storage: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        storage.check()?;
        if !matches!(&*source, SourcePlan::Join { .. }) {
            self.atoms[self.atom_count] = Some(source);
            self.atom_count += 1;
            return Ok(());
        }
        let parts = source.split_join(storage)?;
        self.collect(parts.left, predicates, originals, output, storage)?;
        self.collect(parts.right, predicates, originals, output, storage)?;
        if let Some(on) = parts.on {
            move_conjuncts(on, predicates, originals, output, storage)?;
        }
        self.shells[self.shell_count] = Some(parts.shell);
        self.shell_count += 1;
        Ok(())
    }

    fn materialize(
        &mut self,
        tree: &JoinPlan,
        nodes: &mut impl Iterator<Item = (usize, Range<usize>)>,
        predicates: &mut PredicateBindings,
        output: &mut impl PredicateOutput,
        storage: &ProductionControl<'_>,
    ) -> Result<MutableSourcePlan, SQLError> {
        storage.check()?;
        let (Some(left), Some(right)) = (&tree.left, &tree.right) else {
            assert!(
                tree.left.is_none() && tree.right.is_none(),
                "selected join children are paired"
            );
            let index = tree.relations.trailing_zeros() as usize;
            return Ok(self.atoms[index].take().expect("selected source atom"));
        };
        let strategy = match (tree.join_edge, tree.kind) {
            (None, _) => JoinExecutionStrategy::Auto,
            (Some(_), Some(OperatorKind::HashJoinInner)) => JoinExecutionStrategy::Hash,
            (Some(edge), Some(OperatorKind::IndexJoin)) => JoinExecutionStrategy::IndexNestedLoop {
                inner_column: edge
                    .lookup_column(right.relations)
                    .expect("selected inner key"),
            },
            _ => {
                return Err(SQLError::Internal(
                    "DPccp selected an invalid executable equijoin strategy".into(),
                ))
            }
        };
        let left = self.materialize(left, nodes, predicates, output, storage)?;
        let right = self.materialize(right, nodes, predicates, output, storage)?;
        let (node, range) = nodes.next().expect("selected join predicates");
        let mut children = predicates.take(range);
        let on = match children.len() {
            0 => None,
            1 => children.next(),
            _ => Some(output.conjunction(node, children, storage)?),
        };
        self.shell_count -= 1;
        self.shells[self.shell_count]
            .take()
            .expect("selected join shell")
            .assemble(left, right, on, strategy, storage)
    }
}

fn move_conjuncts(
    expression: OwnedScalarExpr,
    predicates: &mut PredicateBindings,
    originals: &mut impl Iterator<Item = u64>,
    output: &mut impl PredicateOutput,
    storage: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    storage.check()?;
    if let ScalarExpr::And(items) = &*expression {
        let memory = output.take_conjunction_memory(items);
        for child in expression.into_children() {
            move_conjuncts(child, predicates, originals, output, storage)?;
        }
        drop(memory);
    } else {
        predicates.set(
            originals.next().expect("original join predicate"),
            expression,
        );
    }
    Ok(())
}
