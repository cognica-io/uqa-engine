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

use super::scalar::{eval_physical, eval_physical_call_arguments, PhysicalEvalContext};
use super::{
    plpgsql_exec, run_alter_sequence, run_alter_table, run_create_index, run_create_sequence,
    run_create_table, run_create_table_as, run_delete, run_drop, run_explain, run_insert,
    run_merge, run_update, select, Engine,
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
        select::execute_query_plan(self.engine, query, self.params)
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
        query: &QueryPlan,
        or_replace: bool,
    ) -> Result<SQLResult, SQLError> {
        if self.engine.has_table(name) {
            return Err(SQLError::Unsupported(format!(
                "CREATE VIEW: relation `{name}` already exists as a table"
            )));
        }
        if !or_replace && self.engine.view_plan(name).is_some() {
            return Err(SQLError::Unsupported(format!(
                "CREATE VIEW: relation `{name}` already exists"
            )));
        }
        self.engine.register_view_plan(name, query.clone());
        Ok(SQLResult::empty())
    }

    fn execute_show_variable(&self, name: &str) -> SQLResult {
        let mut row = ResultRow::new();
        row.insert(
            name.to_string(),
            Value::Str(self.engine.show_variable(name)),
        );
        SQLResult {
            columns: vec![name.to_string()],
            rows: vec![row],
            affected_rows: 0,
        }
    }

    fn execute_explain(&self, body: &UnifiedPlan) -> Result<SQLResult, SQLError> {
        run_explain(self.engine, body, self.params)
    }

    fn execute_truncate(&self, tables: &[String]) -> Result<SQLResult, SQLError> {
        for table in tables {
            if !self.engine.has_table(table) {
                return Err(SQLError::Unsupported(format!(
                    "TRUNCATE TABLE: relation `{table}` does not exist"
                )));
            }
            self.engine.truncate_table(table)?;
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
            .register_prepared_plan(name.to_string(), body.clone());
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
        let scope = select::CteScope::new();
        let hook = select::ScopedEngineHook::new(self.engine, &scope);
        let context = PhysicalEvalContext::new(None, self.params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        let args = eval_physical_call_arguments(arguments, &context)?;
        plpgsql_exec::run_call(self.engine, name, &args)
    }

    fn execute_command(&self, command: &CommandPlan) -> Result<SQLResult, SQLError> {
        match command {
            CommandPlan::CreateTable(statement) => run_create_table(self.engine, statement.clone()),
            CommandPlan::CreateIndex(statement) => run_create_index(self.engine, statement.clone()),
            CommandPlan::Insert(plan) => self.execute_insert(plan),
            CommandPlan::Update(plan) => self.execute_update(plan),
            CommandPlan::Delete(plan) => self.execute_delete(plan),
            CommandPlan::Drop(statement) => run_drop(self.engine, statement.clone()),
            CommandPlan::AlterTable(statement) => {
                run_alter_table(self.engine, (**statement).clone())
            }
            CommandPlan::CreateView {
                name,
                query,
                or_replace,
            } => self.execute_create_view(name, query, *or_replace),
            CommandPlan::CreateSchema {
                name,
                if_not_exists,
            } => {
                self.engine.register_schema(name, *if_not_exists);
                Ok(SQLResult::empty())
            }
            CommandPlan::SetVariable { name, value } => {
                self.engine.set_variable(name, value);
                Ok(SQLResult::empty())
            }
            CommandPlan::ShowVariable { name } => Ok(self.execute_show_variable(name)),
            CommandPlan::Discard { target } => {
                self.engine.discard(*target);
                Ok(SQLResult::empty())
            }
            CommandPlan::Explain { body, .. } => self.execute_explain(body),
            CommandPlan::Analyze { table } => {
                self.engine.run_analyze(table.as_deref());
                Ok(SQLResult::empty())
            }
            CommandPlan::Truncate { tables, .. } => self.execute_truncate(tables),
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
                query,
            } => run_create_table_as(
                self.engine,
                name.clone(),
                *if_not_exists,
                query,
                self.params,
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
            CommandPlan::DoBlock { language, body } => {
                plpgsql_exec::run_do_block(self.engine, language, body)
            }
            CommandPlan::Call { name, args } => self.execute_call(name, args),
        }
    }
}
