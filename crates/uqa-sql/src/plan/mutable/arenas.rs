//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar-query compaction keeps the existing remapping algorithm and retires child owners with their inputs.

use super::{
    memory::{RewriteField, RewriteMemory},
    MutableCommandPlan, MutableQueryPlan,
};
use crate::{
    catalog::work::{self, Work},
    plan::{subqueries, QueryPlan},
    SQLError,
};
use uqa_core::memory::{BudgetedVec, ProductionControl};

mod nodes;
mod owner;
mod transfer;
pub use nodes::ScalarArenaTransform;
pub use owner::ScalarQueryArena;

impl MutableQueryPlan {
    pub(super) async fn prune_subqueries_from<W: Work + Send>(
        &mut self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        self.assert_owner(control);
        control.check()?;
        if subqueries::query_arena(&mut self.query.root).is_empty() {
            return Ok(());
        }
        crate::construction::retain_with(*control, || async {
            let Some(remap) = subqueries::prepare_query_from(&self.query, *control, work).await?
            else {
                return Ok(());
            };
            subqueries::rewrite_query_from(&mut self.query, &remap, control, work).await?;
            compact(
                subqueries::query_arena(&mut self.query.root),
                self.memory.as_mut().map(query_fields),
                &|index| remap.keep(index),
                Some(control),
                work,
            )
            .await
        })?
        .await
    }

    /// Compact the local scalar-query arena through SQL's existing namespace-aware remapping. Unreachable child payloads release before their constructor records, while the original arena capacity remains owned by the parent.
    pub fn prune_subqueries(mut self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        self.prune_subqueries_in_place(control)?;
        Ok(self)
    }

    pub(super) fn prune_subqueries_in_place(
        &mut self,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.assert_owner(control);
        control.check()?;
        let fields = self.memory.as_mut().map(query_fields);
        super::super::subqueries::prune_query_with_retention(
            &mut self.query,
            *control,
            |arena, keep| {
                work::inline(compact(arena, fields, keep, None, &mut ()))
                    .expect("immediate compaction has no fallible commit operation");
            },
        )?;
        Ok(())
    }
}

async fn compact<W: Work, K: Fn(usize) -> bool + ?Sized>(
    arena: &mut Vec<QueryPlan>,
    mut fields: Option<&mut BudgetedVec<RewriteField>>,
    keep: &K,
    control: Option<&ProductionControl<'_>>,
    work: &mut W,
) -> Result<(), SQLError> {
    let mut retained = 0;
    for index in 0..arena.len() {
        if let Some(control) = control {
            control.check()?;
        }
        work.checkpoint().await?;
        if let Some(control) = control {
            control.check()?;
        }
        if keep(index) {
            arena.swap(retained, index);
            if let Some(fields) = &mut fields {
                fields.swap(retained, index);
            }
            retained += 1;
        }
    }
    arena.truncate(retained);
    if let Some(fields) = fields {
        fields.truncate(retained);
    }
    Ok(())
}

impl MutableCommandPlan {
    /// Compact a mutation's scalar-query arena using its existing source-then-expression namespace remapping. Command CTEs and retained source queries keep their independent owners.
    pub fn prune_subqueries(mut self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        if self.command.scalar_subqueries().is_empty() {
            return Ok(self);
        }
        let fields = self.memory.as_mut().map(|memory| {
            let arena = memory
                .field_mut("value0")
                .as_mut()
                .expect("mutation body")
                .field_mut("subqueries")
                .as_mut()
                .expect("mutation arena");
            match arena {
                RewriteMemory::Fields { fields, .. } => fields,
                RewriteMemory::Scalar(_) | RewriteMemory::Scalars { .. } => {
                    unreachable!("query arena constructor")
                }
            }
        });
        super::super::subqueries::prune_command_with_retention(
            &mut self.command,
            *control,
            |arena, keep| {
                work::inline(compact(arena, fields, keep, None, &mut ()))
                    .expect("immediate compaction has no fallible commit operation");
            },
        )?;
        Ok(self)
    }
}

fn query_fields(memory: &mut RewriteMemory) -> &mut BudgetedVec<RewriteField> {
    let root = memory.field_mut("root").as_mut().expect("query root");
    let block = match root {
        RewriteMemory::Fields { fields, .. } => fields.iter().any(|field| field.name == "block"),
        RewriteMemory::Scalar(_) | RewriteMemory::Scalars { .. } => {
            unreachable!("query root constructor")
        }
    };
    let root = if block {
        root.field_mut("block").as_mut().expect("query block")
    } else {
        root
    };
    match root
        .field_mut("subqueries")
        .as_mut()
        .expect("scalar query arena")
    {
        RewriteMemory::Fields { fields, .. } => fields,
        RewriteMemory::Scalar(_) | RewriteMemory::Scalars { .. } => {
            unreachable!("query arena constructor")
        }
    }
}

#[cfg(test)]
mod tests;
