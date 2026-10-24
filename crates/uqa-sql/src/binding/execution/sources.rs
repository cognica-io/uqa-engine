//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer freshly bound derived-source metadata along its borrowed execution tree.

use std::{collections::BTreeMap, ptr::NonNull, sync::Arc};

use crate::{
    plan::{QueryBlockPlan, RelationalPlan, SourcePlan},
    RowSchema,
};

/// Metadata from one successful execution-source binding. The owner must keep the catalog, CTE definitions, routines and parameters unchanged while constructing the corresponding physical source. This is not a prepared-plan cache or pre-optimization annotation.
pub struct ExecutionSourceBindings<'a> {
    source: &'a SourcePlan,
    children: Option<Box<(Arc<Self>, Arc<Self>)>>,
    query: Option<QuerySourceBinding<'a>>,
}

struct QuerySourceBinding<'a> {
    block: &'a QueryBlockPlan,
    schema: RowSchema,
    outer: Option<RowSchema>,
    sources: Option<Arc<ExecutionSourceBindings<'a>>>,
}

impl ExecutionSourceBindings<'_> {
    /// A rewritten or copied source must obtain its own binding.
    pub fn matches_source(&self, source: &SourcePlan) -> bool {
        std::ptr::eq(self.source, source)
    }

    pub fn join_child(&self, right: bool) -> Option<Arc<Self>> {
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
    ) -> Option<(RowSchema, Option<Arc<Self>>)> {
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
    blocks: BTreeMap<NonNull<QueryBlockPlan>, Option<(RowSchema, Option<RowSchema>)>>,
}

impl CapturedSources {
    pub(super) fn for_source(source: &SourcePlan) -> Self {
        let mut result = Self::default();
        result.collect(source);
        result
    }

    pub(super) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    fn collect(&mut self, source: &SourcePlan) {
        match source {
            SourcePlan::Join { left, right, .. } => {
                self.collect(left);
                self.collect(right);
            }
            SourcePlan::Subquery { body, .. } if body.ctes.is_empty() => {
                if let RelationalPlan::QueryBlock(block) = &body.root {
                    if !block
                        .from
                        .as_ref()
                        .is_some_and(super::source_binds_routines)
                    {
                        self.blocks.insert(NonNull::from(block.as_ref()), None);
                        if let Some(source) = &block.from {
                            self.collect(source);
                        }
                    }
                }
            }
            _ => {}
        }
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

    pub(super) fn take<'a>(&mut self, source: &'a SourcePlan) -> Arc<ExecutionSourceBindings<'a>> {
        let children = match source {
            SourcePlan::Join { left, right, .. } => {
                Some(Box::new((self.take(left), self.take(right))))
            }
            _ => None,
        };
        let query = match source {
            SourcePlan::Subquery { body, .. } => match &body.root {
                RelationalPlan::QueryBlock(block) => self
                    .blocks
                    .remove(&NonNull::from(block.as_ref()))
                    .flatten()
                    .map(|(schema, outer)| QuerySourceBinding {
                        block,
                        schema,
                        outer,
                        sources: block.from.as_ref().map(|source| self.take(source)),
                    }),
                _ => None,
            },
            _ => None,
        };
        Arc::new(ExecutionSourceBindings {
            source,
            children,
            query,
        })
    }
}
