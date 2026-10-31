//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep DISTINCT grouping identity at the analyzed, pre-optimization boundary.

use super::{AnalysisMap, NonNull, ParameterTypes, SQLError, SQLParam};
use crate::plan::{QueryBlockPlan, QueryPlan, RelationalPlan};
use crate::{routines::RoutineResolution, RowSchema};
use uqa_core::memory::ProductionControl;

#[derive(Default)]
pub(super) struct GroupingInputs(
    AnalysisMap<NonNull<QueryBlockPlan>, (RowSchema, Option<RowSchema>)>,
);

impl ParameterTypes<'_> {
    pub(in crate::binding::preparation) fn retain_distinct_grouping(
        &mut self,
        block: &QueryBlockPlan,
        input: &RowSchema,
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        if block.group_distinct {
            if let Some(schemas) = self.query_schemas_mut() {
                schemas
                    .groupings
                    .0
                    .insert(NonNull::from(block), (input.clone(), outer.cloned()))?;
            }
        }
        Ok(())
    }
}

impl GroupingInputs {
    pub(super) fn new(control: &ProductionControl<'_>) -> Self {
        Self(AnalysisMap::new(control))
    }
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn metadata(
        &mut self,
        query: &QueryPlan,
        routines: &dyn RoutineResolution,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<crate::plan::QueryMetadataUpdate, SQLError> {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            return Ok(crate::plan::QueryMetadataUpdate::default());
        };
        let Some((input, outer)) = self.0.remove(&NonNull::from(block.as_ref())) else {
            return Ok(crate::plan::QueryMetadataUpdate::default());
        };
        let grouping = crate::semantics::grouping_sets::analyze_distinct_grouping_with_control(
            routines,
            block,
            &input,
            outer.as_ref(),
            params,
            control,
        )?;
        Ok(crate::plan::QueryMetadataUpdate {
            grouping: Some(grouping),
            ..Default::default()
        })
    }

    pub(super) fn apply_block(
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
}
