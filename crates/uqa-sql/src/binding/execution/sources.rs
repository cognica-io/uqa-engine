//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer freshly bound derived-source metadata along its borrowed execution tree.

use crate::{
    plan::{QueryBlockPlan, RelationalPlan, SourcePlan},
    RowSchema, SQLError,
};
use std::{ptr::NonNull, sync::Arc};
use uqa_core::memory::{OwnedMap, Produced, ProductionControl};

pub type SharedExecutionSourceBindings<'a> = Arc<Produced<ExecutionSourceBindings<'a>>>;
type SourceChildren<'a> = (
    SharedExecutionSourceBindings<'a>,
    SharedExecutionSourceBindings<'a>,
);

/// Metadata from one successful execution-source binding. The owner must keep the catalog, CTE definitions, routines and parameters unchanged while constructing the corresponding physical source. This is not a prepared-plan cache or pre-optimization annotation.
pub struct ExecutionSourceBindings<'a> {
    source: &'a SourcePlan,
    children: Option<Produced<Box<SourceChildren<'a>>>>,
    query: Option<QuerySourceBinding<'a>>,
}

struct QuerySourceBinding<'a> {
    block: &'a QueryBlockPlan,
    schema: RowSchema,
    outer: Option<RowSchema>,
    sources: Option<SharedExecutionSourceBindings<'a>>,
}

impl<'a> ExecutionSourceBindings<'a> {
    /// A rewritten or copied source must obtain its own binding.
    pub fn matches_source(&self, source: &SourcePlan) -> bool {
        std::ptr::eq(self.source, source)
    }

    pub fn join_child(&self, right: bool) -> Option<SharedExecutionSourceBindings<'a>> {
        self.children.as_ref().map(|children| {
            if right {
                children.1.clone()
            } else {
                children.0.clone()
            }
        })
    }

    /// Transfer a derived block's raw source schema only under its complete original enclosing descriptor. Identity checks borrow live syntax; no address is retained beyond that syntax's lifetime.
    pub fn query_source(
        &self,
        block: &QueryBlockPlan,
        outer: Option<&RowSchema>,
        defer_distinct_limit: bool,
    ) -> Option<(RowSchema, Option<SharedExecutionSourceBindings<'a>>)> {
        let query = self.query.as_ref()?;
        if defer_distinct_limit
            || !std::ptr::eq(query.block, block)
            || query.outer.as_ref() != outer
        {
            return None;
        }
        Some((query.schema.clone(), query.sources.clone()))
    }
}

/// Addresses exist only while collecting metadata from the original borrowed tree. Temporary view, CTE and function-member syntax cannot replace an original node, and extraction returns borrowed structural owners rather than address keys.
#[derive(Default)]
pub(in crate::binding) struct CapturedSources {
    blocks: OwnedMap<NonNull<QueryBlockPlan>, Option<(RowSchema, Option<RowSchema>)>>,
}

impl CapturedSources {
    pub(super) fn for_source(
        source: &SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut result = Self::default();
        result.collect(source, control)?;
        Ok(result)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    fn collect(
        &mut self,
        source: &SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        match source {
            SourcePlan::Join { left, right, .. } => {
                self.collect(left, control)?;
                self.collect(right, control)?;
            }
            SourcePlan::Subquery { body, .. } if body.ctes.is_empty() => {
                if let RelationalPlan::QueryBlock(block) = &body.root {
                    if !block
                        .from
                        .as_ref()
                        .is_some_and(super::source_binds_routines)
                    {
                        let key = NonNull::from(block.as_ref());
                        match control.budget() {
                            Some(budget) => {
                                self.blocks.insert_budgeted(key, None, budget)?;
                            }
                            None => {
                                self.blocks.insert(key, None);
                            }
                        }
                        if let Some(source) = &block.from {
                            self.collect(source, control)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(in crate::binding) fn record(
        &mut self,
        block: &QueryBlockPlan,
        schema: &RowSchema,
        outer: Option<&RowSchema>,
    ) {
        if !schema.columns_are_open(None) {
            if let Some(slot) = self.blocks.get_mut(&NonNull::from(block)) {
                *slot = Some((schema.clone(), outer.cloned()));
            }
        }
    }

    pub(super) fn take<'a>(
        &mut self,
        source: &'a SourcePlan,
        control: &ProductionControl<'_>,
    ) -> Result<SharedExecutionSourceBindings<'a>, SQLError> {
        control.check()?;
        let children = match source {
            SourcePlan::Join { left, right, .. } => {
                let left = self.take(left, control)?;
                let right = self.take(right, control)?;
                let memory = control.reserve(size_of::<SourceChildren<'a>>())?;
                Some(control.finish(Box::new((left, right)), memory)?)
            }
            _ => None,
        };
        let query = match source {
            SourcePlan::Subquery { body, .. } => match &body.root {
                RelationalPlan::QueryBlock(block) => self
                    .blocks
                    .remove(&NonNull::from(block.as_ref()))
                    .flatten()
                    .map(|(schema, outer)| {
                        Ok::<_, SQLError>(QuerySourceBinding {
                            block,
                            schema,
                            outer,
                            sources: block
                                .from
                                .as_ref()
                                .map(|source| self.take(source, control))
                                .transpose()?,
                        })
                    })
                    .transpose()?,
                _ => None,
            },
            _ => None,
        };
        Ok(control
            .finish(
                ExecutionSourceBindings {
                    source,
                    children,
                    query,
                },
                control.empty_reservation(),
            )?
            .into_shared(control)?)
    }
}
