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
use uqa_sql::ast::{CreateForeignServer, CreateForeignTable};
use uqa_sql::{ResultRow, SQLError, SQLParam, SQLResult};

use crate::engine_session::{MaterializedViewRegistration, ViewRegistration};

use super::scalar::{
    analyze_physical_call_arguments, eval_physical, eval_physical_call_arguments,
    PhysicalEvalContext,
};
use super::{
    plpgsql_exec, run_alter_sequence, run_alter_table, run_create_index, run_create_sequence,
    run_create_table, run_create_table_as, run_delete, run_drop, run_explain, run_insert,
    run_merge, run_update, select, CreateTableAsExecution, Engine,
};

/// Owns top-level plan orchestration. Relational, mutation, DDL, procedural,
/// and prepared-plan execution all enter through this exhaustive dispatcher;
/// leaf executors never choose a second top-level SQL path.
pub(super) struct UnifiedPlanExecutor<'engine, 'params> {
    engine: &'engine Engine,
    params: &'params [SQLParam],
}

impl<'engine, 'params> UnifiedPlanExecutor<'engine, 'params> {
    pub(super) fn new(engine: &'engine Engine, params: &'params [SQLParam]) -> Self {
        Self { engine, params }
    }

    pub(super) fn execute(&mut self, plan: &UnifiedPlan) -> Result<SQLResult, SQLError> {
        self.engine.cancellation_token().check()?;
        match plan {
            UnifiedPlan::Query(query) => self.execute_query(query),
            UnifiedPlan::Command(command) => self.execute_command(command),
        }
    }

    fn execute_query(&self, query: &QueryPlan) -> Result<SQLResult, SQLError> {
        let mut ctes = select::CteScope::new();
        select::execute_query_plan_with_ctes(self.engine, query, self.params, &mut ctes)
    }

    pub(super) fn execute_query_to_spill(
        &self,
        plan: &UnifiedPlan,
    ) -> Result<select::QueryOutput, SQLError> {
        self.engine.cancellation_token().check()?;
        let UnifiedPlan::Query(query) = plan else {
            return Err(SQLError::Unsupported(
                "SQL cursor accepts exactly one query statement".into(),
            ));
        };
        let mut ctes = select::CteScope::new();
        select::execute_query_plan_output(
            self.engine,
            query,
            self.params,
            &mut ctes,
            select::QueryOutputMode::SharedSpill,
        )
    }

    fn execute_insert(&self, plan: &InsertPlan) -> Result<SQLResult, SQLError> {
        run_insert(self.engine, plan.clone(), self.params)
    }

    fn execute_update(&self, plan: &UpdatePlan) -> Result<SQLResult, SQLError> {
        run_update(self.engine, plan.clone(), self.params)
    }

    fn execute_delete(&self, plan: &DeletePlan) -> Result<SQLResult, SQLError> {
        run_delete(self.engine, plan.clone(), self.params)
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
            Value::Str(self.engine.show_variable(name)?),
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
            let result = UnifiedPlanExecutor::new(self.engine, self.params).execute(body)?;
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

    fn execute_truncate(
        &self,
        tables: &[uqa_sql::ast::TruncateTarget],
        cascade: bool,
        restart_identity: bool,
    ) -> Result<SQLResult, SQLError> {
        let mut targets = std::collections::BTreeSet::new();
        for requested in tables {
            let table = self
                .engine
                .try_resolve_table_name(&requested.table)
                .map_err(|err| {
                    SQLError::Internal(format!("resolve table `{}`: {err}", requested.table))
                })?
                .ok_or_else(|| {
                    SQLError::Unsupported(format!(
                        "TRUNCATE TABLE: relation `{}` does not exist",
                        requested.table
                    ))
                })?;
            let hierarchy = self
                .engine
                .try_table_hierarchy(&table)
                .map_err(|err| SQLError::Internal(format!("read table hierarchy: {err}")))?;
            if !requested.include_descendants && hierarchy.partition_spec.is_some() {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: "cannot truncate only a partitioned table".into(),
                });
            }
            targets.extend(
                self.engine
                    .hierarchy_scan_tables(&table, requested.include_descendants)?,
            );
        }
        if cascade {
            let mut pending = targets.iter().cloned().collect::<Vec<_>>();
            while let Some(table) = pending.pop() {
                for (referrer, _) in self
                    .engine
                    .referrers_to(&table)
                    .map_err(|err| SQLError::Internal(format!("read foreign keys: {err}")))?
                {
                    if targets.insert(referrer.clone()) {
                        pending.push(referrer);
                    }
                }
            }
        } else {
            for table in &targets {
                if let Some((referrer, _)) = self
                    .engine
                    .referrers_to(table)
                    .map_err(|err| SQLError::Internal(format!("read foreign keys: {err}")))?
                    .into_iter()
                    .find(|(referrer, _)| !targets.contains(referrer))
                {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot truncate `{table}` because `{referrer}` references it; truncate both tables or use CASCADE"
                    )));
                }
            }
        }
        let truncate = |engine: &Engine| {
            // Referencing relations first makes the mutation order explicit
            // even though the low-level clear does not evaluate row FKs.
            fn visit(
                engine: &Engine,
                table: &str,
                targets: &std::collections::BTreeSet<String>,
                visiting: &mut std::collections::BTreeSet<String>,
                visited: &mut std::collections::BTreeSet<String>,
                ordered: &mut Vec<String>,
            ) -> Result<(), SQLError> {
                if visited.contains(table) || !visiting.insert(table.to_string()) {
                    return Ok(());
                }
                for (referrer, _) in engine
                    .referrers_to(table)
                    .map_err(|err| SQLError::Internal(format!("read foreign keys: {err}")))?
                {
                    if targets.contains(&referrer) {
                        visit(engine, &referrer, targets, visiting, visited, ordered)?;
                    }
                }
                visiting.remove(table);
                if visited.insert(table.to_string()) {
                    ordered.push(table.to_string());
                }
                Ok(())
            }
            let mut ordered = Vec::with_capacity(targets.len());
            let mut visiting = std::collections::BTreeSet::new();
            let mut visited = std::collections::BTreeSet::new();
            for table in &targets {
                visit(
                    engine,
                    table,
                    &targets,
                    &mut visiting,
                    &mut visited,
                    &mut ordered,
                )?;
            }
            engine.truncate_tables_with_identity(&ordered, restart_identity)
        };
        if self.engine.transaction_depth() == 0 {
            self.engine.transaction(truncate)?;
        } else {
            truncate(self.engine)?;
        }
        Ok(SQLResult::empty())
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
        let scope = select::CteScope::new();
        let hook = select::ScopedEngineHook::new(self.engine, &scope);
        let context = PhysicalEvalContext::new(None, self.params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        let bound: Vec<SQLParam> = params
            .iter()
            .map(|expression| eval_physical(expression, &context).map(SQLParam::Scalar))
            .collect::<Result<_, _>>()?;
        UnifiedPlanExecutor::new(self.engine, &bound).execute(&plan)
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
        self.engine
            .register_foreign_table(
                statement.name.clone(),
                statement.server_name.clone(),
                statement.columns.clone(),
                statement.options.clone(),
                statement.if_not_exists,
            )
            .map_err(SQLError::Unsupported)?;
        Ok(SQLResult::empty())
    }

    fn execute_merge(&self, plan: &MergePlan) -> Result<SQLResult, SQLError> {
        run_merge(self.engine, plan.clone(), self.params)
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
        let scope = select::CteScope::new();
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
        plpgsql_exec::run_call(self.engine, name, &args, &argument_types, explicit_variadic)
    }

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
            CommandPlan::GrantRoutine(statement) => {
                self.engine.grant_sql_routine(statement)?;
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
            CommandPlan::AlterTable(statement) => {
                run_alter_table(self.engine, (**statement).clone())
            }
            CommandPlan::AlterViewOptions(statement) => {
                self.engine.alter_view_options(statement)?;
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
            } => {
                self.engine
                    .register_schema(name, *if_not_exists)
                    .map_err(|err| {
                        SQLError::Internal(format!("CREATE SCHEMA catalog write failed: {err}"))
                    })?;
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
                self.engine
                    .run_analyze(table.as_deref())
                    .map_err(|err| SQLError::Internal(format!("ANALYZE failed: {err}")))?;
                Ok(SQLResult::empty())
            }
            CommandPlan::Truncate {
                tables,
                cascade,
                restart_identity,
            } => self.execute_truncate(tables, *cascade, *restart_identity),
            CommandPlan::Transaction(statement) => {
                self.engine.run_transaction_statement(statement.clone())?;
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
                plpgsql_exec::run_do_block(self.engine, language, body)
            }
            CommandPlan::Call { name, args } => self.execute_call(name, args),
        }
    }
}
