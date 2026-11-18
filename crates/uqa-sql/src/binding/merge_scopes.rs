//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE expressions resolve against their candidate row's relation namespace.

use super::{RowSchema, SchemaScope};
use crate::plan::MergePlan;
use crate::routines::RoutineResolution;
use crate::{SQLError, SQLParam};

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
        let target = RowSchema::with_relation_qualifier(target, qualifier);
        let source = self.bind_source(routines, &merge.source, &merge.subqueries, params, None)?;
        self.bind_expression_type(
            routines,
            &merge.join_condition,
            joined,
            &merge.subqueries,
            params,
        )?;
        if let Some(predicate) = &merge.target_predicate {
            self.bind_expression_type(routines, predicate, joined, &merge.subqueries, params)?;
        }
        for clause in &merge.when_clauses {
            let input = clause.input_scope(joined, &target, &source);
            for expression in clause.expressions() {
                self.bind_expression_type(routines, expression, input, &merge.subqueries, params)?;
            }
        }
        Ok(())
    }
}
