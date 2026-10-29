//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consuming query edits retain each constructor's independent allocation ownership.

use super::{copy::Copying, OwnedScalarExpr, QueryPlan, RelationalPlan};
use crate::{SQLError, ScalarExpr};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

mod analysis;
mod arenas;
pub(crate) use analysis::{QueryMetadataUpdate, SourceMetadataUpdate};
pub use arenas::{ScalarArenaTransform, ScalarQueryArena};
pub(crate) mod commands;
pub(crate) mod construction;
mod ctes;
mod deferred;
pub use deferred::OwnedDeferredRewriteIter;
mod executable;
mod expression;
pub use expression::MutableExpressionPlan;
mod grouping;
pub(crate) mod memory;
mod metadata;
pub(crate) mod nodes;
mod parents;
pub(crate) mod projections;
mod queries;
mod scoped;
mod slots;
pub use scoped::OwnedScopedPlanVisitor;
mod sources;
mod traversal;
mod vectors;
mod windows;
pub use commands::{
    CommandAssignmentSlot, CommandQuerySlot, CommandScalarSlot, MutableCommandPlan,
};
pub use executable::MutableExecutablePlan;
use memory::RewriteMemory;
pub use projections::{OwnedProjection, OwnedProjectionIter};
pub use queries::QueryChildSlot;
pub use slots::QueryScalarSlot;
pub use sources::{
    MutableJoinParts, MutableJoinShell, MutableSourcePlan, SourceChildSlot, SourceScalarSlot,
};

/// A private query destination with independently owned scalar fields and query children. Observation is read-only; consuming edits transfer or destroy the corresponding constructor records together with their IR.
#[derive(Debug)]
pub struct MutableQueryPlan {
    query: QueryPlan,
    memory: Option<RewriteMemory>,
}

impl QueryPlan {
    /// Copy a complete query while keeping mutable constructor ownership separate. Every destination and ownership-record buffer is admitted before allocation; published immutable schemas retain their original shared owners.
    pub fn copy_for_rewrite(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableQueryPlan, SQLError> {
        control.check()?;
        let Some(mut copying) = Copying::for_rewrite(*control) else {
            return Ok(MutableQueryPlan {
                query: self.clone(),
                memory: None,
            });
        };
        let query = copying.query_fields(self)?;
        let output = MutableQueryPlan {
            query,
            memory: Some(copying.into_rewrite_memory()),
        };
        control.check()?;
        Ok(output)
    }
}

impl MutableQueryPlan {
    pub(in crate::plan) fn from_rewrite_parts(
        query: QueryPlan,
        memory: Option<RewriteMemory>,
    ) -> Self {
        Self { query, memory }
    }

    pub fn reserved_bytes(&self) -> usize {
        self.memory
            .as_ref()
            .map_or(0, RewriteMemory::reserved_bytes)
    }

    /// Rewrite one selected scalar root without copying parent containers or siblings. An absent optional scalar is left absent. Error or unwind destroys the complete private query.
    pub fn map_scalar(
        mut self,
        slot: QueryScalarSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(expression) = slot.expression_mut(&mut self.query) else {
            return Ok(self);
        };
        let memory_slot = self.memory.as_mut().map(|memory| slot.memory_mut(memory));
        let scalar_memory = match memory_slot {
            Some(memory) => match memory.take().expect("present scalar ownership") {
                RewriteMemory::Scalar(memory) => Some(memory),
                RewriteMemory::Fields { .. } => unreachable!("scalar root constructor"),
            },
            None => None,
        };
        let input = OwnedScalarExpr::from_rewrite_parts(
            std::mem::replace(expression, ScalarExpr::Literal(Value::Null)),
            scalar_memory,
        );
        let output = input.rewrite(control, map)?;
        let (output, memory) = output.into_rewrite_parts();
        *expression = output;
        if let Some(owner) = &mut self.memory {
            *slot.memory_mut(owner) = memory.map(RewriteMemory::Scalar);
        }
        Ok(self)
    }

    /// Keep selected projection entries in their original vector. Removed expression and alias storage is destroyed before its own constructor records; surviving aliases and scalar children keep their addresses and allowance.
    pub fn retain_projections(
        mut self,
        control: &ProductionControl<'_>,
        mut keep: impl FnMut(usize, &super::ProjectionPlan) -> Result<bool, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "projection retention requires a query block".into(),
            ));
        };
        let mut fields = self.memory.as_mut().map(|memory| {
            let node = memory.field_mut("root").as_mut().expect("query root");
            let node = node.field_mut("block").as_mut().expect("query block");
            match node
                .field_mut("projections")
                .as_mut()
                .expect("projection vector")
            {
                RewriteMemory::Fields { fields, .. } => fields,
                RewriteMemory::Scalar(_) => unreachable!("projection vector owner"),
            }
        });
        let mut retained = 0;
        for index in 0..block.projections.len() {
            control.check()?;
            if keep(index, &block.projections[index])? {
                block.projections.swap(retained, index);
                if let Some(fields) = &mut fields {
                    fields.swap(retained, index);
                }
                retained += 1;
            }
        }
        block.projections.truncate(retained);
        if let Some(fields) = fields {
            fields.truncate(retained);
        }
        control.check()?;
        Ok(self)
    }

    /// Freeze only the surviving query storage. Temporary constructor records release before publication; no size observation or retrospective reservation split is used.
    pub fn finish(self, control: &ProductionControl<'_>) -> Result<Produced<QueryPlan>, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Self { query, memory } = self;
        Ok(control.finish(query, memory.map(RewriteMemory::into_reservation))?)
    }

    fn assert_owner(&self, control: &ProductionControl<'_>) {
        match (&self.memory, control.budget()) {
            (Some(RewriteMemory::Fields { direct, .. }), Some(budget)) => assert!(
                direct.budget().shares_allowance(budget),
                "query rewrite preserves its constructor allowance"
            ),
            (None, None) => {}
            _ => panic!("controlled and ordinary query ownership must not be mixed"),
        }
    }
}

impl std::ops::Deref for MutableQueryPlan {
    type Target = QueryPlan;
    fn deref(&self) -> &QueryPlan {
        &self.query
    }
}

#[cfg(test)]
mod tests;
