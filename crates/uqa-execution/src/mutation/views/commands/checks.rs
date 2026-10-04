//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Check the rows that a view's `INSTEAD OF` triggers return against the check options of the views a statement was rewritten through to reach it.

use super::{target_row, CteScope, SQLError, SQLParam, Value, ViewDmlTarget};
use crate::mutation::{
    constraints::{trigger_view_check_violation, ConstraintContext},
    expressions::eval_mutation_expr,
    rows::context::MutationExpressionContext,
};
use uqa_sql::plan::ViewCheckPlan;

/// A statement that automatically updatable views rewrote to `target`, a view whose `INSTEAD OF` triggers store its rows.
pub struct TriggerViewChecks<'a, S: Clone + 'static> {
    pub expressions: MutationExpressionContext<'a, S>,
    /// The catalog and authority that describe a row a check option rejects.
    pub constraints: ConstraintContext<'a>,
    pub target: &'a ViewDmlTarget,
    pub target_qualifier: &'a str,
    /// The check options of the views the statement was rewritten through, inner views first.
    pub checks: &'a [ViewCheckPlan],
    /// The columns of `target` the statement inserts or updates.
    pub supplied: &'a [String],
    pub params: &'a [SQLParam],
    pub scope: &'a CteScope<S>,
}

impl<S: Clone + 'static> TriggerViewChecks<'_, S> {
    /// Check the row a trigger returned, which `ExecInsert` and `ExecUpdateEpilogue` pass to `ExecWithCheckOptions` after the trigger has stored it: a check that is not true rejects the row.
    pub fn validate(&self, values: &[Value]) -> Result<(), SQLError> {
        if self.checks.is_empty() {
            return Ok(());
        }
        let row = target_row(self.target, self.target_qualifier, values)?;
        for check in self.checks {
            let value = eval_mutation_expr(
                self.expressions,
                self.scope,
                &check.predicate,
                Some(&row),
                self.params,
            )?;
            if !uqa_sql::expr::truthy(&value) {
                return Err(trigger_view_check_violation(
                    self.constraints,
                    &check.view,
                    self.target,
                    self.supplied,
                    values,
                ));
            }
        }
        Ok(())
    }
}
