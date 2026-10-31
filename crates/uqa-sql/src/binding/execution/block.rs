//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution binding retains original syntax or one private mutable query constructor through physical assembly.

use crate::{
    plan::{MutableQueryPlan, QueryBlockPlan, RelationalPlan},
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam,
};
use std::borrow::Cow;
use uqa_core::memory::{Produced, ProductionControl};

enum Storage<'a> {
    Borrowed(&'a QueryBlockPlan),
    Owned(Box<Produced<MutableQueryPlan>>),
}

pub struct ExecutionQueryBlock<'a>(Storage<'a>);

impl<'a> ExecutionQueryBlock<'a> {
    pub fn borrowed(block: &'a QueryBlockPlan) -> Self {
        Self(Storage::Borrowed(block))
    }

    pub(super) fn owned(
        query: MutableQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let memory = control.reserve(size_of::<Produced<MutableQueryPlan>>())?;
        Ok(Self(Storage::Owned(Box::new(
            control.finish(query, memory)?,
        ))))
    }

    fn into_owned(self, control: &ProductionControl<'_>) -> Result<MutableQueryPlan, SQLError> {
        control.check()?;
        match self.0 {
            Storage::Borrowed(block) => block.copy_for_rewrite(control),
            Storage::Owned(query) => {
                let (query, memory) = (*query).into_parts();
                drop(memory);
                Ok(query)
            }
        }
    }

    pub fn bind_grouping_names(
        self,
        routines: &dyn RoutineResolution,
        schema: &RowSchema,
        outer: Option<&RowSchema>,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let query = crate::semantics::grouping_sets::bind_grouping_names_owned(
            routines,
            self.into_owned(control)?,
            schema,
            outer,
            params,
            control,
        )?;
        Self::owned(query, control)
    }

    pub fn resolve_outer_rewrites(
        self,
        schema: &RowSchema,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if !crate::semantics::view_rewrite::has_resolvable_outer_rewrites(&self, schema) {
            return Ok(self);
        }
        let query = crate::semantics::view_rewrite::resolve_outer_rewrites_owned(
            self.into_owned(control)?,
            schema,
            control,
        )?;
        Self::owned(query, control)
    }

    pub(super) fn into_uncontrolled(self) -> Cow<'a, QueryBlockPlan> {
        match self.0 {
            Storage::Borrowed(block) => Cow::Borrowed(block),
            Storage::Owned(query) => {
                let query = (*query)
                    .into_uncontrolled()
                    .expect("ordinary execution query owner");
                let query = query
                    .finish(&ProductionControl::uncontrolled())
                    .expect("ordinary execution query")
                    .into_uncontrolled()
                    .expect("ordinary query fields");
                let RelationalPlan::QueryBlock(block) = query.root else {
                    unreachable!("execution query block")
                };
                Cow::Owned(*block)
            }
        }
    }
}

impl AsRef<QueryBlockPlan> for ExecutionQueryBlock<'_> {
    fn as_ref(&self) -> &QueryBlockPlan {
        match &self.0 {
            Storage::Borrowed(block) => block,
            Storage::Owned(query) => {
                let RelationalPlan::QueryBlock(block) = &query.root else {
                    unreachable!("execution query block")
                };
                block
            }
        }
    }
}
impl std::ops::Deref for ExecutionQueryBlock<'_> {
    type Target = QueryBlockPlan;
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}
