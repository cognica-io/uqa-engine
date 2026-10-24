//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep DISTINCT grouping identity at the analyzed, pre-optimization boundary.

use super::{BTreeMap, NonNull, ParameterTypes, SQLError, SQLParam, UnifiedPlan};
use crate::plan::{QueryBlockPlan, QueryPlan, RelationalPlan};
use crate::{routines::RoutineResolution, RowSchema};

#[derive(Default)]
pub(super) struct GroupingInputs(BTreeMap<NonNull<QueryBlockPlan>, (RowSchema, Option<RowSchema>)>);

impl ParameterTypes<'_> {
    pub(in crate::binding::preparation) fn retain_distinct_grouping(
        &mut self,
        block: &QueryBlockPlan,
        input: &RowSchema,
        outer: Option<&RowSchema>,
    ) {
        if block.group_distinct {
            if let Some(schemas) = self.query_schemas_mut() {
                schemas
                    .groupings
                    .0
                    .insert(NonNull::from(block), (input.clone(), outer.cloned()));
            }
        }
    }
}

impl GroupingInputs {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn apply_block(
        &mut self,
        query: &mut QueryPlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            return Ok(());
        };
        let Some((input, outer)) = self.0.remove(&NonNull::from(&**block)) else {
            return Ok(());
        };
        // All input constants and selected call bindings have been installed. No scalar optimization has run, so equal values cannot conflate different analyzed expressions.
        query.analyzed_grouping = Some(crate::semantics::grouping_sets::analyze_distinct_grouping(
            routines,
            block,
            &input,
            outer.as_ref(),
            params,
        )?);
        Ok(())
    }

    pub(super) fn apply_query(
        &mut self,
        query: &mut QueryPlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        if self.is_empty() {
            return Ok(());
        }
        let mut result = Ok(());
        query.visit_queries_mut(&mut |query| {
            if result.is_ok() {
                result = self.apply_block(query, routines, params);
            }
        });
        result
    }

    pub(super) fn apply_plan(
        &mut self,
        plan: &mut UnifiedPlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        if self.is_empty() {
            return Ok(());
        }
        let mut result = Ok(());
        plan.visit_queries_mut(&mut |query| {
            if result.is_ok() {
                result = self.apply_block(query, routines, params);
            }
        });
        result
    }
}
