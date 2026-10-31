//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred substitutions move with their exact item owners while source and scalar arenas retain their original namespaces.

use super::{
    Copying, MutableQueryPlan, RelationalPlan, RewriteMemory, ScalarArenaTransform,
    ScalarQueryArena,
};
use crate::{semantics::view_rewrite::DeferredOuterRewrite, SQLError};
use uqa_core::memory::{Budgeted, Produced, ProductionControl};

pub struct OwnedDeferredRewriteIter {
    values: std::vec::IntoIter<DeferredOuterRewrite>,
    memory: Option<RewriteMemory>,
    index: usize,
}
impl Iterator for OwnedDeferredRewriteIter {
    type Item = Produced<DeferredOuterRewrite>;
    fn next(&mut self) -> Option<Self::Item> {
        let value = self.values.next()?;
        let owner = self
            .memory
            .as_mut()
            .and_then(|memory| memory.item_mut(self.index).take())
            .map(RewriteMemory::into_reservation);
        self.index += 1;
        Some(match owner {
            Some(memory) => Produced::from(Budgeted::new(value, memory)),
            None => ProductionControl::uncontrolled()
                .finish(value, None)
                .expect("ordinary deferred substitution"),
        })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl ExactSizeIterator for OwnedDeferredRewriteIter {}

impl MutableQueryPlan {
    pub fn take_deferred_rewrites(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, OwnedDeferredRewriteIter), SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "deferred substitutions require a query block".into(),
            ));
        };
        let values = std::mem::take(&mut block.deferred_outer_rewrites);
        let memory = self.memory.as_mut().and_then(|memory| {
            block_memory(memory)
                .field_mut("deferred_outer_rewrites")
                .take()
        });
        Ok((
            self,
            OwnedDeferredRewriteIter {
                values: values.into_iter(),
                memory,
                index: 0,
            },
        ))
    }

    pub fn replace_deferred_rewrites(
        mut self,
        rewrites: impl ExactSizeIterator<Item = Produced<DeferredOuterRewrite>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (values, owner) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let values = copying.map(rewrites, |copying, rewrite| {
                copying.take_deferred_rewrite(rewrite)
            })?;
            (values, Some(copying.into_rewrite_memory()))
        } else {
            let mut values = Vec::with_capacity(rewrites.len());
            for rewrite in rewrites {
                control.check()?;
                values.push(
                    rewrite
                        .into_uncontrolled()
                        .expect("ordinary deferred substitution"),
                );
            }
            (values, None)
        };
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "deferred substitutions require a query block".into(),
            ));
        };
        block.deferred_outer_rewrites = values;
        if let Some(memory) = &mut self.memory {
            *block_memory(memory).field_mut("deferred_outer_rewrites") = owner;
        }
        Ok(self)
    }

    /// Edit block-owned roots while lending their scalar-query namespace. FROM expressions and nested query bodies retain their separate rewrite boundary.
    pub fn transform_root_scalar_arenas(
        mut self,
        control: &ProductionControl<'_>,
        transform: &mut ScalarArenaTransform<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let mut arena = self.take_arena();
        super::nodes::query_scalars(
            &mut self.query,
            self.memory.as_mut(),
            false,
            control,
            &mut |expression, memory, control| {
                super::OwnedScalarExpr::transform_nodes_in_place(
                    expression,
                    memory,
                    control,
                    &mut |identity, expression| transform(identity, expression, &mut arena),
                )
            },
        )?;
        self.restore_arena(arena);
        Ok(self)
    }

    pub fn map_source_with_arena(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(
            super::MutableSourcePlan,
            &mut ScalarQueryArena,
        ) -> Result<super::MutableSourcePlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let mut arena = self.take_arena();
        self = self.map_source(control, |source| map(source, &mut arena))?;
        self.restore_arena(arena);
        Ok(self)
    }

    pub(super) fn take_arena(&mut self) -> ScalarQueryArena {
        let queries = std::mem::take(arena_queries(&mut self.query.root));
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| arena_memory(memory).take());
        ScalarQueryArena { queries, memory }
    }
    pub(super) fn restore_arena(&mut self, arena: ScalarQueryArena) {
        *arena_queries(&mut self.query.root) = arena.queries;
        if let Some(memory) = &mut self.memory {
            *arena_memory(memory) = arena.memory;
        }
    }
}

fn block_memory(memory: &mut RewriteMemory) -> &mut RewriteMemory {
    memory
        .field_mut("root")
        .as_mut()
        .expect("query root")
        .field_mut("block")
        .as_mut()
        .expect("query block")
}
fn arena_queries(root: &mut RelationalPlan) -> &mut Vec<crate::plan::QueryPlan> {
    match root {
        RelationalPlan::QueryBlock(block) => &mut block.subqueries,
        RelationalPlan::SetOp { subqueries, .. } | RelationalPlan::Values { subqueries, .. } => {
            subqueries
        }
    }
}
fn arena_memory(memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
    let root = memory.field_mut("root").as_mut().expect("query root");
    let is_block = match root {
        RewriteMemory::Fields { fields, .. } => fields.iter().any(|field| field.name == "block"),
        RewriteMemory::Scalar(_) => unreachable!(),
    };
    let root = if is_block {
        root.field_mut("block").as_mut().expect("query block")
    } else {
        root
    };
    root.field_mut("subqueries")
}
