//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exhaustive physical executor for the unified SQL plan.

use uqa_core::Value;
use uqa_planner::{
    CommandPlan, DeletePlan, ExpressionPlan, InsertPlan, MergePlan, QueryPlan, UnifiedPlan,
    UpdatePlan,
};
use uqa_sql::ast::{CreateForeignServer, CreateForeignTable, FunctionParamMode};
use uqa_sql::{ResultRow, SQLError, SQLParam, SQLResult};

use crate::engine_capabilities::{MutationCoordinator, QueryRuntimeView, SessionExecutionView};
use crate::engine_session::{MaterializedViewRegistration, ViewRegistration};

use super::scalar::{
    analyze_physical_call_arguments, eval_physical, eval_physical_call_arguments,
    PhysicalEvalContext,
};
use super::{
    plpgsql_exec, run_alter_sequence, run_alter_table, run_create_index, run_create_sequence,
    run_create_table, run_create_table_as, run_delete, run_drop, run_explain, run_insert,
    run_merge, run_update, run_vacuum, select, CreateTableAsExecution, Engine,
};

fn call_output_schema(
    engine: &Engine,
    definition: &uqa_sql::ast::CreateFunction,
    parameter_types: &[String],
) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
    let output_indices = definition
        .params
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| {
            matches!(
                parameter.mode,
                FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    if output_indices.is_empty() {
        return Ok(None);
    }
    let columns = definition
        .output_params()
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            if parameter.name.is_empty() {
                format!("column{}", index + 1)
            } else {
                parameter.name.clone()
            }
        })
        .collect::<Vec<_>>();
    let column_types = output_indices
        .into_iter()
        .map(|index| {
            super::resolve_catalog_column_type(engine, &parameter_types[index])
                .or_else(|| uqa_sql::ast::ColumnType::from_sql_name(&parameter_types[index]).ok())
                .map(Some)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!("unknown type `{}`", parameter_types[index]))
                })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(Some(uqa_execution::RowSchema::with_types(
        columns,
        column_types,
    )))
}

pub(super) fn analyze_call_result_schema(
    engine: &Engine,
    name: &str,
    arguments: &[ExpressionPlan],
    params: &[SQLParam],
) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
    if arguments
        .iter()
        .any(|argument| !argument.subqueries.is_empty())
    {
        return Err(SQLError::Unsupported(
            "cannot use subquery in CALL argument".into(),
        ));
    }
    let (call_arguments, explicit_variadic) = analyze_physical_call_arguments(arguments)?;
    let argument_names = call_arguments
        .iter()
        .map(|argument| argument.name.map(str::to_string))
        .collect::<Vec<_>>();
    let scope = select::CteScope::new_for_current_routine(engine);
    let argument_types = arguments
        .iter()
        .zip(&call_arguments)
        .map(|(argument, call_argument)| {
            if matches!(
                call_argument.value,
                uqa_execution::ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
                Ok(None)
            } else {
                select::bind_expression_plan_type(engine, argument, params, &scope)
            }
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let Some(resolved) = engine.resolve_static_sql_routine_match(
        name,
        None,
        &argument_names,
        &argument_types,
        explicit_variadic,
        crate::engine_user_functions::RoutineCallKind::Procedure,
    )?
    else {
        let signature = argument_types
            .iter()
            .map(|argument| {
                argument
                    .as_ref()
                    .map_or_else(|| "unknown", super::column_type_name)
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!("procedure {name}({signature}) does not exist"),
        });
    };
    call_output_schema(
        engine,
        &resolved.function.def,
        &resolved.invocation.parameter_types,
    )
}

/// Owns top-level plan orchestration. Relational, mutation, DDL, procedural,
/// and prepared-plan execution all enter through this exhaustive dispatcher;
/// leaf executors never choose a second top-level SQL path.
pub(super) struct UnifiedPlanExecutor<'engine, 'params> {
    engine: &'engine Engine,
    session: SessionExecutionView<'engine>,
    runtime: QueryRuntimeView<'engine>,
    mutation: MutationCoordinator<'engine>,
    params: &'params [SQLParam],
    nested_statement: bool,
    privilege_subject: Option<String>,
}

impl<'engine, 'params> UnifiedPlanExecutor<'engine, 'params> {
    pub(super) fn new(engine: &'engine Engine, params: &'params [SQLParam]) -> Self {
        Self::with_nested_statement(engine, params, false)
    }

    pub(super) fn new_nested(engine: &'engine Engine, params: &'params [SQLParam]) -> Self {
        Self::with_nested_statement(engine, params, true)
    }

    pub(super) fn with_nested_statement(
        engine: &'engine Engine,
        params: &'params [SQLParam],
        nested_statement: bool,
    ) -> Self {
        Self {
            engine,
            session: engine.session_execution_view(),
            runtime: engine.query_runtime_view(),
            mutation: engine.mutation_coordinator(),
            params,
            nested_statement,
            privilege_subject: None,
        }
    }

    pub(super) fn with_privilege_subject(mut self, subject: &str) -> Self {
        self.privilege_subject = Some(subject.to_string());
        self
    }

    pub(super) fn execute(&mut self, plan: &UnifiedPlan) -> Result<SQLResult, SQLError> {
        self.runtime.check_cancelled()?;
        super::read_only::validate_transaction_plan(self.engine, plan)?;
        match plan {
            UnifiedPlan::Query(query) => self.execute_query(query),
            UnifiedPlan::Command(command) => self.execute_command(command),
        }
    }

    fn execute_query(&self, query: &QueryPlan) -> Result<SQLResult, SQLError> {
        if self.session.transaction_depth() != 0 {
            select::lock_query_relations(self.engine, query)?;
        }
        let mut ctes =
            select::CteScope::new_for_statement(self.engine, self.privilege_subject.as_deref());
        select::execute_query_plan_with_ctes(self.engine, query, self.params, &mut ctes)
    }

    pub(super) fn execute_query_to_spill(
        &self,
        plan: &UnifiedPlan,
    ) -> Result<select::QueryOutput, SQLError> {
        self.runtime.check_cancelled()?;
        super::read_only::validate_transaction_plan(self.engine, plan)?;
        let UnifiedPlan::Query(query) = plan else {
            return Err(SQLError::Unsupported(
                "SQL cursor accepts exactly one query statement".into(),
            ));
        };
        if self.session.transaction_depth() != 0 {
            select::lock_query_relations(self.engine, query)?;
        }
        let mut ctes =
            select::CteScope::new_for_statement(self.engine, self.privilege_subject.as_deref());
        select::execute_query_plan_output(
            self.engine,
            query,
            self.params,
            &mut ctes,
            select::QueryOutputMode::SharedSpill,
        )
    }

    fn execute_insert(&self, plan: &InsertPlan) -> Result<SQLResult, SQLError> {
        let mut plan = plan.clone();
        self.apply_statement_privilege_subject(
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        );
        run_insert(self.engine, plan, self.params)
    }

    fn execute_update(&self, plan: &UpdatePlan) -> Result<SQLResult, SQLError> {
        let mut plan = plan.clone();
        self.apply_statement_privilege_subject(
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        );
        run_update(self.engine, plan, self.params)
    }

    fn execute_delete(&self, plan: &DeletePlan) -> Result<SQLResult, SQLError> {
        let mut plan = plan.clone();
        self.apply_statement_privilege_subject(
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        );
        run_delete(self.engine, plan, self.params)
    }

    fn apply_statement_privilege_subject(
        &self,
        statement_subject: &mut Option<String>,
        target_subject: &mut Option<String>,
    ) {
        let Some(subject) = self.privilege_subject.as_ref() else {
            return;
        };
        statement_subject.get_or_insert_with(|| subject.clone());
        target_subject.get_or_insert_with(|| subject.clone());
    }

    fn execute_create_view(
        &self,
        name: &str,
        column_names: &[String],
        query: &QueryPlan,
        or_replace: bool,
        persistence: uqa_sql::ast::RelationPersistence,
        options: &[(String, String)],
    ) -> Result<SQLResult, SQLError> {
        self.engine.register_view_plan(ViewRegistration {
            name,
            column_names,
            plan: query.clone(),
            or_replace,
            persistence,
            options,
            params: self.params,
        })?;
        Ok(SQLResult::empty())
    }

    fn execute_show_variable(&self, name: &str) -> Result<SQLResult, SQLError> {
        let mut row = ResultRow::new();
        row.insert(
            name.to_string(),
            Value::Str(self.session.show_variable(name)?),
        );
        Ok(SQLResult {
            columns: vec![name.to_string()],
            column_types: vec![Some(uqa_sql::ColumnType::Text)],
            rows: vec![row],
            positional_rows: None,
            affected_rows: 0,
        })
    }

    fn execute_explain(
        &self,
        body: &UnifiedPlan,
        analyze: bool,
        verbose: bool,
        format: Option<&str>,
    ) -> Result<SQLResult, SQLError> {
        let analysis = if analyze {
            let started = std::time::Instant::now();
            let mut executor = UnifiedPlanExecutor::new_nested(self.engine, self.params);
            executor
                .privilege_subject
                .clone_from(&self.privilege_subject);
            let result = executor.execute(body)?;
            let rows = u64::try_from(result.rows.len())
                .map_err(|_| SQLError::Internal("EXPLAIN ANALYZE row count exceeds u64".into()))?;
            Some(super::select::ExplainAnalysis {
                elapsed: started.elapsed(),
                rows,
                affected_rows: result.affected_rows,
            })
        } else {
            None
        };
        run_explain(body, verbose, format, analysis.as_ref())
    }

    fn execute_prepare(&self, name: &str, body: &UnifiedPlan) -> Result<SQLResult, SQLError> {
        if self.engine.lookup_prepared(name).is_some() {
            return Err(SQLError::Unsupported(format!(
                "Prepared statement `{name}` already exists"
            )));
        }
        self.engine
            .register_prepared_plan(name.to_string(), body.clone())?;
        Ok(SQLResult::empty())
    }

    fn execute_prepared(
        &self,
        name: &str,
        params: &[ExpressionPlan],
    ) -> Result<SQLResult, SQLError> {
        let plan = self.engine.lookup_prepared(name).ok_or_else(|| {
            SQLError::Unsupported(format!("Prepared statement `{name}` does not exist"))
        })?;
        let scope = select::CteScope::new_for_current_routine(self.engine);
        let hook = select::ScopedEngineHook::new(self.engine, &scope);
        let context = PhysicalEvalContext::new(None, self.params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        let bound: Vec<SQLParam> = params
            .iter()
            .map(|expression| eval_physical(expression, &context).map(SQLParam::Scalar))
            .collect::<Result<_, _>>()?;
        UnifiedPlanExecutor::new_nested(self.engine, &bound).execute(&plan)
    }

    fn execute_deallocate(&self, name: Option<&str>) -> Result<SQLResult, SQLError> {
        if let Some(name) = name {
            if self.engine.lookup_prepared(name).is_none() {
                return Err(SQLError::Unsupported(format!(
                    "Prepared statement `{name}` does not exist"
                )));
            }
        }
        self.engine.deallocate_prepared(name);
        Ok(SQLResult::empty())
    }

    fn execute_create_foreign_server(
        &self,
        statement: &CreateForeignServer,
    ) -> Result<SQLResult, SQLError> {
        self.engine
            .register_foreign_server(
                statement.name.clone(),
                statement.fdw_type.clone(),
                statement.options.clone(),
                statement.if_not_exists,
            )
            .map_err(SQLError::Unsupported)?;
        Ok(SQLResult::empty())
    }

    fn execute_create_foreign_table(
        &self,
        statement: &CreateForeignTable,
    ) -> Result<SQLResult, SQLError> {
        self.engine.register_foreign_table_with_checks(
            statement.name.clone(),
            statement.server_name.clone(),
            statement.columns.clone(),
            statement.checks.clone(),
            statement.options.clone(),
            statement.if_not_exists,
        )?;
        Ok(SQLResult::empty())
    }

    fn execute_merge(&self, plan: &MergePlan) -> Result<SQLResult, SQLError> {
        let mut plan = plan.clone();
        self.apply_statement_privilege_subject(
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        );
        run_merge(self.engine, plan, self.params)
    }

    fn execute_call(
        &self,
        name: &str,
        arguments: &[ExpressionPlan],
    ) -> Result<SQLResult, SQLError> {
        if arguments
            .iter()
            .any(|argument| !argument.subqueries.is_empty())
        {
            return Err(SQLError::Unsupported(
                "cannot use subquery in CALL argument".into(),
            ));
        }
        let scope = select::CteScope::new_for_current_routine(self.engine);
        let (call_arguments, explicit_variadic) = analyze_physical_call_arguments(arguments)?;
        let argument_types = arguments
            .iter()
            .zip(&call_arguments)
            .map(|(argument, call_argument)| {
                let value = call_argument.value;
                if matches!(
                    value,
                    uqa_execution::ScalarExpr::Literal(Value::Str(_) | Value::Null)
                ) {
                    Ok(None)
                } else {
                    select::bind_expression_plan_type(self.engine, argument, self.params, &scope)
                }
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let hook = select::ScopedEngineHook::new(self.engine, &scope);
        let context = PhysicalEvalContext::new(None, self.params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        let args = eval_physical_call_arguments(arguments, &context)?;
        plpgsql_exec::run_call(
            self.engine,
            name,
            &args,
            &argument_types,
            explicit_variadic,
            self.nested_statement,
        )
    }

    fn execute_create_schema(
        &self,
        name: &str,
        if_not_exists: bool,
    ) -> Result<SQLResult, SQLError> {
        self.engine.prepare_explicit_transaction_writer()?;
        let role_owner = self.engine.session_execution_view().current_user();
        self.engine.ensure_database_privilege(
            &role_owner,
            crate::engine_database_security::DatabaseAclPrivilege::Create,
        )?;
        self.mutation
            .register_schema(name, if_not_exists, &role_owner)
            .map_err(|error| {
                SQLError::Internal(format!("CREATE SCHEMA catalog write failed: {error}"))
            })?;
        Ok(SQLResult::empty())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    fn execute_command(&self, command: &CommandPlan) -> Result<SQLResult, SQLError> {
        match command {
            CommandPlan::CreateTable(statement) => {
                run_create_table(self.engine, statement.as_ref().clone())
            }
            CommandPlan::CreateIndex(statement) => run_create_index(self.engine, statement.clone()),
            CommandPlan::Insert(plan) => self.execute_insert(plan),
            CommandPlan::Update(plan) => self.execute_update(plan),
            CommandPlan::Delete(plan) => self.execute_delete(plan),
            CommandPlan::Drop(statement) => run_drop(self.engine, statement.clone()),
            CommandPlan::AlterRoutineOwner(statement) => {
                self.engine.alter_sql_routine_owner(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::RenameRoutine(statement) => {
                self.engine.rename_sql_routine(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantRoutine(statement) => {
                self.engine.grant_sql_routine(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantTable(statement) => {
                self.engine.grant_table_privileges(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantSequence(statement) => {
                self.engine.grant_sequence_privileges(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantDatabase(statement) => {
                self.engine.grant_database_privileges(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantSchema(statement) => {
                self.engine.grant_schema_privileges(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantRole(statement) => {
                self.engine.grant_roles(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateRole(statement) => {
                self.engine.create_role(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterRole(statement) => {
                self.engine.alter_role(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DropRole(statement) => {
                self.engine.drop_roles(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateTrigger(statement) => {
                self.engine.register_trigger(statement.clone())?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DropTrigger(statement) => {
                self.engine.drop_trigger_sql(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateRule(statement) => {
                self.engine.register_rule(statement.clone())?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DropRule(statement) => {
                self.engine.drop_rule_sql(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterTable(statement) => {
                run_alter_table(self.engine, (**statement).clone())
            }
            CommandPlan::AlterForeignTable(statement) => {
                self.engine.alter_foreign_table(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterView(statement) => {
                self.engine.alter_view(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateView {
                name,
                column_names,
                query,
                or_replace,
                persistence,
                options,
            } => self.execute_create_view(
                name,
                column_names,
                query,
                *or_replace,
                *persistence,
                options,
            ),
            CommandPlan::CreateMaterializedView {
                name,
                column_names,
                if_not_exists,
                with_no_data,
                options,
                query,
            } => {
                self.engine
                    .register_materialized_view_plan(MaterializedViewRegistration {
                        name,
                        column_names,
                        plan: (**query).clone(),
                        if_not_exists: *if_not_exists,
                        with_no_data: *with_no_data,
                        options,
                        params: self.params,
                    })?;
                Ok(SQLResult::empty())
            }
            CommandPlan::RefreshMaterializedView {
                name,
                concurrently,
                with_no_data,
            } => {
                self.engine
                    .refresh_materialized_view(name, *concurrently, *with_no_data)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateSchema {
                name,
                if_not_exists,
            } => self.execute_create_schema(name, *if_not_exists),
            CommandPlan::Notify { channel, payload } => {
                self.engine.notify(channel, payload)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::Listen { channel } => {
                self.engine.listen(channel)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::Unlisten { channel } => {
                self.engine.unlisten(channel.as_deref())?;
                Ok(SQLResult::empty())
            }
            CommandPlan::SetVariable { name, value } => {
                if name.eq_ignore_ascii_case("role") {
                    self.engine.set_role(value)?;
                } else {
                    self.engine.set_variable(name, value)?;
                }
                Ok(SQLResult::empty())
            }
            CommandPlan::ResetVariable { name } => {
                if name.eq_ignore_ascii_case("role") {
                    self.engine.set_role("default")?;
                } else {
                    self.engine.reset_variable(name)?;
                }
                Ok(SQLResult::empty())
            }
            CommandPlan::ResetAllVariables => {
                self.engine.reset_all_variables();
                Ok(SQLResult::empty())
            }
            CommandPlan::SetConstraints {
                constraints,
                deferred,
            } => {
                self.engine
                    .set_constraints(constraints, *deferred, self.nested_statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::ShowVariable { name } => self.execute_show_variable(name),
            CommandPlan::Discard { target } => {
                self.engine.discard(*target)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::Load { library } => {
                self.engine.load_library(library)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::Explain {
                analyze,
                verbose,
                format,
                body,
            } => self.execute_explain(body, *analyze, *verbose, format.as_deref()),
            CommandPlan::Analyze { table } => {
                let targets = if let Some(requested) = table.as_deref() {
                    let Some((canonical, "table")) =
                        self.engine.try_resolve_visible_relation_kind(requested)?
                    else {
                        return Err(SQLError::UnknownTable(requested.to_string()));
                    };
                    self.engine.ensure_table_privilege(
                        &canonical,
                        crate::engine_table_security::TableAclPrivilege::Maintain,
                    )?;
                    vec![canonical]
                } else {
                    self.engine.maintenance_table_names("analyze")?
                };
                for target in targets {
                    self.engine
                        .run_analyze_target(&target, &[], true)
                        .map_err(|err| SQLError::Internal(format!("ANALYZE failed: {err}")))?;
                }
                Ok(SQLResult::empty())
            }
            CommandPlan::Vacuum(statement) => run_vacuum(self.engine, statement),
            CommandPlan::Truncate {
                tables,
                cascade,
                restart_identity,
            } => crate::engine_truncate::execute_sql_truncate(
                self.engine,
                tables,
                *cascade,
                *restart_identity,
            ),
            CommandPlan::Transaction(statement) => {
                self.engine.run_transaction_statement(statement.clone())?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DeclareCursor {
                name,
                binary,
                scroll,
                hold,
                query,
            } => super::session_portal_worker::declare_session_portal(
                self.engine,
                self.params,
                name,
                *binary,
                *scroll,
                *hold,
                query,
            ),
            CommandPlan::FetchCursor(fetch) => self.engine.fetch_session_portal(fetch),
            CommandPlan::CloseCursor { name } => {
                if let Some(name) = name {
                    self.engine.close_session_portal(name)?;
                } else {
                    self.engine.close_all_session_portals();
                }
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateSequence(statement) => {
                run_create_sequence(self.engine, statement.clone())
            }
            CommandPlan::AlterSequence(statement) => {
                run_alter_sequence(self.engine, statement.clone())
            }
            CommandPlan::CreateTableAs {
                name,
                if_not_exists,
                column_names,
                with_no_data,
                persistence,
                on_commit,
                query,
            } => run_create_table_as(
                self.engine,
                CreateTableAsExecution {
                    name,
                    if_not_exists: *if_not_exists,
                    column_names,
                    with_no_data: *with_no_data,
                    persistence: *persistence,
                    on_commit: *on_commit,
                    query,
                    params: self.params,
                },
            ),
            CommandPlan::Prepare { name, body } => self.execute_prepare(name, body),
            CommandPlan::Execute { name, params } => self.execute_prepared(name, params),
            CommandPlan::Deallocate { name } => self.execute_deallocate(name.as_deref()),
            CommandPlan::CreateForeignServer(statement) => {
                self.execute_create_foreign_server(statement)
            }
            CommandPlan::CreateForeignTable(statement) => {
                self.execute_create_foreign_table(statement)
            }
            CommandPlan::Merge(plan) => self.execute_merge(plan),
            CommandPlan::CreateFunction(definition) => {
                plpgsql_exec::run_create_function(self.engine, (**definition).clone())
            }
            CommandPlan::DropFunction(statement) => {
                plpgsql_exec::run_drop_function(self.engine, statement)
            }
            CommandPlan::AlterRoutine(statement) => {
                self.engine.alter_sql_routine(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DoBlock { language, body } => {
                plpgsql_exec::run_do_block(self.engine, language, body, self.nested_statement)
            }
            CommandPlan::Call { name, args } => self.execute_call(name, args),
        }
    }
}
