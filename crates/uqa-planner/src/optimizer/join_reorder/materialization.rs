//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reassemble an eligible source tree using its existing leaves and boxed nodes.

use super::{
    JoinExecutionStrategy, JoinGraphError, JoinGraphResult, JoinPredicateBinding, ScalarExpr,
    SourcePlan,
};

pub(super) fn reorder(
    source: &mut SourcePlan,
    tree: &crate::JoinPlan,
    predicates: &mut [JoinPredicateBinding],
) -> JoinGraphResult<SourcePlan> {
    let mut sources = Sources {
        atoms: std::array::from_fn(|_| None),
        boxes: std::array::from_fn(|_| None),
        atom_count: 0,
        box_count: 0,
    };
    let mut original = predicates
        .iter_mut()
        .filter(|predicate| predicate.expression.is_none());
    sources.collect(take_source(source), &mut original);
    assert!(
        original.next().is_none(),
        "all original join predicates were moved"
    );
    sources.materialize(tree)
}

struct Sources {
    atoms: [Option<Box<SourcePlan>>; 64],
    boxes: [Option<Box<SourcePlan>>; 62],
    atom_count: usize,
    box_count: usize,
}

impl Sources {
    fn collect<'a>(
        &mut self,
        source: SourcePlan,
        predicates: &mut impl Iterator<Item = &'a mut JoinPredicateBinding>,
    ) {
        let SourcePlan::Join {
            left, right, on, ..
        } = source
        else {
            unreachable!("selected source root has at least two atoms");
        };
        self.collect_box(left, predicates);
        self.collect_box(right, predicates);
        if let Some(on) = on {
            move_conjuncts(on, predicates);
        }
    }

    fn collect_box<'a>(
        &mut self,
        mut source: Box<SourcePlan>,
        predicates: &mut impl Iterator<Item = &'a mut JoinPredicateBinding>,
    ) {
        if matches!(source.as_ref(), SourcePlan::Join { .. }) {
            self.collect(take_source(&mut source), predicates);
            self.boxes[self.box_count] = Some(source);
            self.box_count += 1;
        } else {
            self.atoms[self.atom_count] = Some(source);
            self.atom_count += 1;
        }
    }

    fn boxed(&mut self, tree: &crate::JoinPlan) -> JoinGraphResult<Box<SourcePlan>> {
        if tree.left.is_none() && tree.right.is_none() {
            return self.atom(tree);
        }
        let source = self.materialize(tree)?;
        self.box_count -= 1;
        let mut node = self.boxes[self.box_count]
            .take()
            .expect("collected source node");
        *node = source;
        Ok(node)
    }

    fn atom(&mut self, tree: &crate::JoinPlan) -> JoinGraphResult<Box<SourcePlan>> {
        let source_id = usize::try_from(tree.relations.trailing_zeros()).map_err(|_| {
            JoinGraphError::InvalidPlan {
                detail: "join leaf index exceeds the addressable source range".into(),
            }
        })?;
        self.atoms
            .get_mut(source_id)
            .and_then(Option::take)
            .ok_or_else(|| JoinGraphError::InvalidPlan {
                detail: format!(
                    "join source id {source_id} is unavailable in {} source atom(s)",
                    self.atom_count
                ),
            })
    }

    fn materialize(&mut self, tree: &crate::JoinPlan) -> JoinGraphResult<SourcePlan> {
        match (&tree.left, &tree.right) {
            (None, None) => self.atom(tree).map(|source| *source),
            (Some(left), Some(right)) => {
                let strategy = match (tree.join_edge, tree.kind) {
                    (None, _) => JoinExecutionStrategy::Auto,
                    (Some(_), Some(crate::OperatorKind::HashJoinInner)) => {
                        JoinExecutionStrategy::Hash
                    }
                    (Some(_), Some(kind)) => {
                        return Err(JoinGraphError::InvalidPlan {
                            detail: format!(
                                "DPccp selected non-executable equijoin strategy {kind:?}"
                            ),
                        })
                    }
                    (Some(_), None) => {
                        return Err(JoinGraphError::InvalidPlan {
                            detail: "DPccp equijoin node has no physical strategy".into(),
                        })
                    }
                };
                Ok(SourcePlan::Join {
                    comma: false,
                    left: self.boxed(left)?,
                    right: self.boxed(right)?,
                    kind: uqa_sql::ast::JoinKind::Inner,
                    on: None,
                    using: None,
                    natural: false,
                    alias: None,
                    column_aliases: Vec::new(),
                    lateral: false,
                    strategy,
                })
            }
            _ => Err(JoinGraphError::InvalidPlan {
                detail: "join node contains exactly one child".into(),
            }),
        }
    }
}

fn move_conjuncts<'a>(
    expression: ScalarExpr,
    predicates: &mut impl Iterator<Item = &'a mut JoinPredicateBinding>,
) {
    if let ScalarExpr::And(items) = expression {
        for item in items {
            move_conjuncts(item, predicates);
        }
    } else {
        predicates
            .next()
            .expect("original join predicate binding")
            .expression = Some(expression);
    }
}

fn take_source(source: &mut SourcePlan) -> SourcePlan {
    std::mem::replace(
        source,
        SourcePlan::Table {
            name: String::new(),
            executable_query: None,
            qualifier: String::new(),
            alias: None,
            column_aliases: Vec::new(),
            bound_columns: None,
            include_descendants: true,
        },
    )
}
