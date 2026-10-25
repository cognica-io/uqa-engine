//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Successful statement completion retains the exact state maintained by execution.

use super::{StatementOutput, UnifiedPlanExecutor};
use uqa_sql::{
    plan::{CommandPlan, UnifiedPlan},
    SQLError, SQLResult,
};

impl<S: Clone + Send + Sync + 'static> UnifiedPlanExecutor<'_, '_, S> {
    pub fn execute_for_output(&mut self, plan: &UnifiedPlan) -> Result<StatementOutput, SQLError> {
        self.execute_for_output_mode(plan, super::QueryOutputMode::SharedSpill)
    }

    /// Consumers share the ordinary statement validation, snapshot and completion boundary. RETURNING reaches a consumer before the next mutation row; ordinary result callers retain its bounded spool.
    pub(super) fn execute_for_output_mode(
        &mut self,
        plan: &UnifiedPlan,
        mode: super::QueryOutputMode<'_, S>,
    ) -> Result<StatementOutput, SQLError> {
        let output = match plan {
            UnifiedPlan::Query(_) => StatementOutput::Query(self.execute_query_output(plan, mode)?),
            UnifiedPlan::Command(command)
                if matches!(
                    command.as_ref(),
                    CommandPlan::Insert(_)
                        | CommandPlan::Update(_)
                        | CommandPlan::Delete(_)
                        | CommandPlan::Merge(_)
                ) =>
            {
                crate::statement::validation::validate_plan(
                    &self.context.validation,
                    self.context.runtime.cancellation,
                    plan,
                )?;
                self.validate_command_target(command)?;
                let consumer = match mode {
                    super::QueryOutputMode::RowConsumer(factory) => Some(factory.bind(None)?),
                    _ => None,
                };
                let schema = if consumer.is_some() {
                    let inputs = self.context.mutations.mutation_initialization_context();
                    crate::mutation::entry::command_returning_schema(
                        &self.context.mutations.returning_execution_context(),
                        self.context.mutations.returning_analysis_context(),
                        inputs.mutation.scopes,
                        command,
                        self.params,
                    )?
                } else {
                    None
                };
                let destination = consumer
                    .as_deref()
                    .zip(schema.as_ref())
                    .map(|(consumer, schema)| {
                        crate::mutation::returning::ReturningDestination::new(consumer, schema)
                    })
                    .transpose()?;
                let result = match command.as_ref() {
                    CommandPlan::Insert(plan) => self.execute_insert(plan, destination),
                    CommandPlan::Update(plan) => self.execute_update(plan, destination),
                    CommandPlan::Delete(plan) => self.execute_delete(plan, destination),
                    CommandPlan::Merge(plan) => self.execute_merge(plan, destination),
                    _ => unreachable!("mutation command selected above"),
                }?;
                StatementOutput::Mutation(result)
            }
            UnifiedPlan::Command(_) => return self.execute(plan).map(StatementOutput::Command),
        };
        self.context
            .validation
            .transactions
            .retain_completed_statement_snapshot();
        Ok(output)
    }

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
