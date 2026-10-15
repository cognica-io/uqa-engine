//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Successful statement completion retains the exact state maintained by execution.

use super::UnifiedPlanExecutor;
use uqa_sql::{plan::UnifiedPlan, SQLError, SQLResult};

impl<S: Clone + Send + Sync + 'static> UnifiedPlanExecutor<'_, '_, S> {
    pub fn execute(&mut self, plan: &UnifiedPlan) -> Result<SQLResult, SQLError> {
        crate::statement::validation::validate_plan(
            &self.context.validation,
            self.context.runtime.cancellation,
            plan,
        )?;
        let transaction_failed = self.context.controls.transaction_failed();
        let mut result = match plan {
            UnifiedPlan::Query(query) => self.execute_query(query),
            UnifiedPlan::Command(command) => self.execute_command(command),
        }?;
        uqa_sql::result::completion::set_command_completion(plan, &mut result, transaction_failed);
        self.context
            .validation
            .transactions
            .retain_completed_statement_snapshot();
        Ok(result)
    }
}
