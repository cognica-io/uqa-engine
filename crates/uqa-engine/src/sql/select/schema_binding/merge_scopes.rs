//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE expressions resolve against their candidate row's relation namespace.

use super::{RowSchema, SchemaScope};
use crate::engine_user_functions::RoutineResolution;
use uqa_planner::{MergePlan, MergeWhenPlan};
use uqa_sql::{SQLError, SQLParam};

impl SchemaScope {
    pub(super) fn bind_merge_expressions(
        &mut self,
        routines: &dyn RoutineResolution,
        merge: &MergePlan,
        params: &[SQLParam],
        target: &RowSchema,
        joined: &RowSchema,
    ) -> Result<(), SQLError> {
        let qualifier = merge
            .target_alias
            .as_deref()
            .unwrap_or(&merge.target_qualifier);
        let target = RowSchema::with_qualified_types(
            qualifier,
            target.columns().to_vec(),
            target.column_types().to_vec(),
        );
        let source = self.bind_source(routines, &merge.source, &merge.subqueries, params, None)?;
        self.bind_expression_type(
            routines,
            &merge.join_condition,
            joined,
            &merge.subqueries,
            params,
            None,
        )?;
        if let Some(predicate) = &merge.target_predicate {
            self.bind_expression_type(
                routines,
                predicate,
                joined,
                &merge.subqueries,
                params,
                None,
            )?;
        }
        for clause in &merge.when_clauses {
            let input = match clause {
                MergeWhenPlan::UpdateNotMatchedBySource { .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                | MergeWhenPlan::NothingNotMatchedBySource { .. } => &target,
                MergeWhenPlan::InsertNotMatched { .. }
                | MergeWhenPlan::NothingNotMatched { .. } => &source,
                _ => joined,
            };
            let (condition, values) = match clause {
                MergeWhenPlan::UpdateMatched {
                    condition,
                    assignments,
                }
                | MergeWhenPlan::UpdateNotMatchedBySource {
                    condition,
                    assignments,
                } => (
                    condition,
                    assignments
                        .iter()
                        .map(|assignment| &assignment.value)
                        .collect::<Vec<_>>(),
                ),
                MergeWhenPlan::InsertNotMatched {
                    condition, values, ..
                } => (condition, values.iter().collect()),
                MergeWhenPlan::DeleteMatched { condition }
                | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                | MergeWhenPlan::NothingMatched { condition }
                | MergeWhenPlan::NothingNotMatched { condition }
                | MergeWhenPlan::NothingNotMatchedBySource { condition } => (condition, Vec::new()),
            };
            for expression in condition.iter().chain(values) {
                self.bind_expression_type(
                    routines,
                    expression,
                    input,
                    &merge.subqueries,
                    params,
                    None,
                )?;
            }
        }
        Ok(())
    }
}
