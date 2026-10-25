//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CommandPlan, SQLError, SQLResult, UnifiedPlan, UnifiedPlanExecutor};
use uqa_sql::explain::ExplainSettings;

mod consumer;
mod serialization;

impl<S: Clone + Send + Sync + 'static> UnifiedPlanExecutor<'_, '_, S> {
    pub(super) fn execute_explain(
        &self,
        body: &UnifiedPlan,
        options: &uqa_sql::explain::ExplainOptions,
        unbound_parameters: Option<&[uqa_sql::SQLParam]>,
    ) -> Result<SQLResult, SQLError> {
        let settings = options.interpret()?;
        if let Some(parameters) = unbound_parameters {
            let mut executor = UnifiedPlanExecutor::new_nested(self.context.clone(), parameters);
            executor
                .privilege_subject
                .clone_from(&self.privilege_subject);
            executor.explain_statement(body, &settings)
        } else {
            self.explain_statement(body, &settings)
        }
    }

    /// EXPLAIN plans a query and optionally runs it with a relation destination; utility statements themselves are not ordinary nested execution.
    fn explain_statement(
        &self,
        body: &UnifiedPlan,
        settings: &ExplainSettings,
    ) -> Result<SQLResult, SQLError> {
        let source;
        let destination;
        let query = match body {
            UnifiedPlan::Query(_) => {
                destination = None;
                body
            }
            UnifiedPlan::Command(command) => match command.as_ref() {
                CommandPlan::Insert(_)
                | CommandPlan::Update(_)
                | CommandPlan::Delete(_)
                | CommandPlan::Merge(_) => {
                    destination = None;
                    body
                }
                CommandPlan::DeclareCursor { query, .. } => {
                    destination = None;
                    source = UnifiedPlan::Query(query.clone());
                    &source
                }
                CommandPlan::CreateTableAs {
                    name,
                    if_not_exists,
                    persistence,
                    query,
                    ..
                } => {
                    if self.context.schemas.tables_as.explain_existing_target(
                        name,
                        *if_not_exists,
                        *persistence,
                    )? {
                        return self.explain_utility(body, settings);
                    }
                    destination = Some(command.as_ref());
                    source = UnifiedPlan::Query(query.clone());
                    &source
                }
                CommandPlan::CreateMaterializedView {
                    name,
                    if_not_exists,
                    query,
                    ..
                } => {
                    if self
                        .context
                        .schemas
                        .views
                        .explain_existing_target(name, *if_not_exists)?
                    {
                        return self.explain_utility(body, settings);
                    }
                    destination = Some(command.as_ref());
                    source = UnifiedPlan::Query(query.clone());
                    &source
                }
                CommandPlan::Execute { name, params } => {
                    let bound = crate::query::prepared::bind_execute_parameters(
                        self.context.prepared.arguments,
                        name,
                        self.context.prepared.state.prepared_parameter_types(name),
                        params,
                    )?;
                    let plan = self
                        .context
                        .prepared
                        .plans
                        .plan_for_execution(name, &bound)?
                        .ok_or_else(|| {
                            uqa_sql::prepared::statement_error("26000", name, "does not exist")
                        })?;
                    let mut executor =
                        UnifiedPlanExecutor::new_nested(self.context.clone(), &bound);
                    executor
                        .privilege_subject
                        .clone_from(&self.privilege_subject);
                    return executor.explain_statement(&plan, settings);
                }
                _ => return self.explain_utility(body, settings),
            },
        };
        self.explain_query(query, destination, settings)
    }

    fn explain_query(
        &self,
        query: &UnifiedPlan,
        destination: Option<&CommandPlan>,
        settings: &ExplainSettings,
    ) -> Result<SQLResult, SQLError> {
        let planning_started = std::time::Instant::now();
        let planned = self
            .context
            .physical_explain
            .plan_explained_statement(query)?;
        let body = &planned;
        if self.context.validation.transactions.transaction_depth() != 0 {
            crate::query::locking::lock_explained_relations(
                self.context.queries.row_lock_context(),
                body,
            )?;
        }
        if let UnifiedPlan::Query(query) = body {
            let ctes = self
                .context
                .queries
                .statement_scope(self.privilege_subject.as_ref());
            crate::query::privileges::routine_calls::query(
                &self.context.queries.query_context().source,
                crate::query::sources::prepared_queries::prepare(
                    &self.context.queries.query_context().source,
                    query,
                    self.params,
                    &ctes,
                )?
                .as_ref(),
                self.params,
                &ctes,
            )?;
        }
        if let UnifiedPlan::Command(command) = body {
            crate::mutation::routine_calls::explain(
                &self.context.mutations.mutation_initialization_context(),
                self.context.mutations.mutation_context().targets,
                command,
                self.params,
            )?;
        }
        let retained = self.context.diagnostics.diagnostics_control()?;
        let control = uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.context.runtime.cancellation,
        );
        let physical = self.context.physical_explain.physical_explain_plan(
            body,
            self.params,
            settings.costs,
            &control,
        )?;
        let estimate = if settings.costs {
            Some(self.context.physical_explain.explained_plan_estimate(
                body,
                self.params,
                &control,
            )?)
        } else {
            None
        };
        let shown = if settings.settings {
            self.context.validation.session.explain_settings(&control)?
        } else {
            control.finish(Vec::new(), control.empty_reservation())?
        };
        let (shown, shown_memory) = shown.into_parts();
        let (mut physical, physical_memory) = physical.into_parts();
        physical.settings = shown;
        physical.estimate = estimate;
        physical.planning_elapsed = Some(planning_started.elapsed());
        let physical = control.finish(physical, control.combine(physical_memory, shown_memory))?;
        let analysis = if settings.analyze {
            Some(self.analyze_explained(body, destination, settings, control)?)
        } else {
            None
        };
        self.render_explanation(body, settings, analysis.as_ref(), &physical)
    }

    fn analyze_explained(
        &self,
        body: &UnifiedPlan,
        destination: Option<&CommandPlan>,
        settings: &ExplainSettings,
        control: uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::result::ExplainAnalysis, SQLError> {
        let diagnostics = self.context.diagnostics.diagnostics_scope()?;
        let started = std::time::Instant::now();
        let mut executor = UnifiedPlanExecutor::new_nested(self.context.clone(), self.params);
        executor
            .privilege_subject
            .clone_from(&self.privilege_subject);
        let (affected_rows, rows, serialization) = if let Some(destination) = destination {
            crate::statement::validation::validate_plan(
                &self.context.validation,
                self.context.runtime.cancellation,
                body,
            )?;
            let result = executor.execute_explained_destination(destination, body)?;
            (
                result.affected_rows,
                result.affected_rows,
                uqa_sql::result::explain::ExplainSerializationMetrics::default(),
            )
        } else {
            executor.consume_explained_output(body, settings, control)?
        };
        Ok(uqa_sql::result::ExplainAnalysis {
            vector_searches: diagnostics.finish()?,
            elapsed: started.elapsed(),
            rows,
            affected_rows,
            serialization,
        })
    }

    fn consume_explained_output(
        &mut self,
        body: &UnifiedPlan,
        settings: &ExplainSettings,
        control: uqa_core::memory::ProductionControl<'_>,
    ) -> Result<
        (
            u64,
            u64,
            uqa_sql::result::explain::ExplainSerializationMetrics,
        ),
        SQLError,
    > {
        let serialization = if settings.serialize == uqa_sql::explain::ExplainSerialization::None {
            None
        } else {
            let scope = self
                .context
                .queries
                .statement_scope(self.privilege_subject.as_ref());
            let hook = self
                .context
                .queries
                .query_context()
                .source
                .relational
                .expression_scope(scope);
            serialization::RowSerialization::new(settings.serialize, settings.timing, hook, control)
        };
        let consumer = std::rc::Rc::new(
            consumer::ExplainConsumer::new(self.context.runtime.cancellation)
                .with_serialization(serialization),
        );
        let output = self.execute_for_output_mode(
            body,
            super::QueryOutputMode::physical_consumer(consumer.clone()),
        )?;
        match output {
            super::StatementOutput::Query(_) => Ok((0, consumer.rows(), consumer.serialization())),
            super::StatementOutput::Mutation(output) => {
                let affected_rows = output.affected_rows;
                Ok((affected_rows, consumer.rows(), consumer.serialization()))
            }
            super::StatementOutput::Command(_) => Err(SQLError::Internal(
                "EXPLAIN query execution returned a utility result".into(),
            )),
        }
    }

    fn explain_utility(
        &self,
        body: &UnifiedPlan,
        settings: &ExplainSettings,
    ) -> Result<SQLResult, SQLError> {
        self.render_explanation(
            body,
            settings,
            None,
            &uqa_sql::result::ExplainPhysicalPlan::default(),
        )
    }

    fn render_explanation(
        &self,
        body: &UnifiedPlan,
        settings: &ExplainSettings,
        analysis: Option<&uqa_sql::result::ExplainAnalysis>,
        physical: &uqa_sql::result::ExplainPhysicalPlan,
    ) -> Result<SQLResult, SQLError> {
        let retained = self.context.diagnostics.diagnostics_control()?;
        let control = uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.context.runtime.cancellation,
        );
        (self.context.explain)(body, settings, analysis, physical, &control)
    }

    fn execute_explained_destination(
        &self,
        destination: &CommandPlan,
        source: &UnifiedPlan,
    ) -> Result<SQLResult, SQLError> {
        let UnifiedPlan::Query(executable) = source else {
            return Err(SQLError::Internal(
                "explained relation destination requires a query".into(),
            ));
        };
        match destination {
            CommandPlan::CreateTableAs {
                name,
                if_not_exists,
                column_names,
                with_no_data,
                persistence,
                on_commit,
                query,
                ..
            } => crate::schema::ctas::entry::run_create_table_as(
                self.context.schemas.tables_as,
                crate::schema::ctas::CreateTableAsExecution {
                    name,
                    if_not_exists: *if_not_exists,
                    column_names,
                    with_no_data: *with_no_data,
                    persistence: *persistence,
                    on_commit: *on_commit,
                    query,
                    executable: Some(executable),
                    params: self.params,
                },
            ),
            CommandPlan::CreateMaterializedView {
                name,
                column_names,
                if_not_exists,
                with_no_data,
                options,
                query,
            } => {
                let rows = crate::schema::view_creation::register_materialized_view_plan(
                    self.context.schemas.views,
                    crate::schema::view_creation::MaterializedViewRegistration {
                        name,
                        column_names,
                        plan: (**query).clone(),
                        executable: Some(executable),
                        if_not_exists: *if_not_exists,
                        with_no_data: *with_no_data,
                        options,
                        params: self.params,
                    },
                )?;
                Ok(SQLResult::from_affected(rows.unwrap_or(0)))
            }
            _ => Err(SQLError::Internal(
                "invalid explained relation destination".into(),
            )),
        }
    }
}
