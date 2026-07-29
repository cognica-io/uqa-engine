//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine implementation of the exhaustive unified SQL plan driver.

use uqa_core::Value;
use uqa_planner::{
    CommandPlan, CtePlan, ExpressionPlan, QueryPlan, SourcePlan, UnifiedPlan, UnifiedPlanDriver,
    UnifiedPlanExecutor,
};
use uqa_sql::ast::{
    CreateForeignServer, CreateForeignTable, DeleteStmt, InsertStmt, MergeStmt, SelectStmt,
    UpdateStmt,
};
use uqa_sql::expr::{eval, EvalContext};
use uqa_sql::{ResultRow, SQLError, SQLParam, SQLResult, Statement};

use super::{
    plpgsql_exec, run_alter_sequence, run_alter_table, run_create_index, run_create_sequence,
    run_create_table, run_create_table_as, run_delete, run_drop, run_explain, run_insert,
    run_merge, run_update, select, Engine,
};

pub(super) struct EngineUnifiedPlanDriver<'engine, 'params> {
    engine: &'engine Engine,
    params: &'params [SQLParam],
}

impl<'engine, 'params> EngineUnifiedPlanDriver<'engine, 'params> {
    pub(super) fn new(engine: &'engine Engine, params: &'params [SQLParam]) -> Self {
        Self { engine, params }
    }

    fn execute_query(&self, query: &QueryPlan) -> Result<SQLResult, SQLError> {
        select::execute_query_plan(self.engine, query, self.params)
    }

    fn execute_insert(
        &self,
        statement: &InsertStmt,
        source: Option<&QueryPlan>,
        expressions: &[ExpressionPlan],
    ) -> Result<SQLResult, SQLError> {
        run_insert(
            self.engine,
            statement.clone(),
            source.cloned(),
            expressions.to_vec(),
            self.params,
        )
    }

    fn execute_update(
        &self,
        statement: &UpdateStmt,
        ctes: &[CtePlan],
        source: Option<&SourcePlan>,
        expressions: &[ExpressionPlan],
    ) -> Result<SQLResult, SQLError> {
        run_update(
            self.engine,
            statement.clone(),
            ctes.to_vec(),
            source.cloned(),
            expressions.to_vec(),
            self.params,
        )
    }

    fn execute_delete(
        &self,
        statement: &DeleteStmt,
        ctes: &[CtePlan],
        source: Option<&SourcePlan>,
        expressions: &[ExpressionPlan],
    ) -> Result<SQLResult, SQLError> {
        run_delete(
            self.engine,
            statement.clone(),
            ctes.to_vec(),
            source.cloned(),
            expressions.to_vec(),
            self.params,
        )
    }

    fn execute_create_view(
        &self,
        name: &str,
        definition: &SelectStmt,
        query: &QueryPlan,
        or_replace: bool,
    ) -> Result<SQLResult, SQLError> {
        if self.engine.has_table(name) {
            return Err(SQLError::Unsupported(format!(
                "CREATE VIEW: relation `{name}` already exists as a table"
            )));
        }
        if !or_replace && self.engine.view(name).is_some() {
            return Err(SQLError::Unsupported(format!(
                "CREATE VIEW: relation `{name}` already exists"
            )));
        }
        self.engine
            .register_view_plan(name, definition.clone(), query.clone());
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
        match body {
            UnifiedPlan::Query(query) => {
                let statement = query.physical_select().ok_or_else(|| {
                    SQLError::Internal("EXPLAIN query has no physical SELECT carrier".into())
                })?;
                run_explain(
                    self.engine,
                    Statement::Select(Box::new(statement)),
                    self.params,
                )
            }
            UnifiedPlan::Command(command) => {
                let mut row = ResultRow::new();
                row.insert("plan".into(), Value::Str(command.name().to_string()));
                Ok(SQLResult::from_rows(vec!["plan".into()], vec![row]))
            }
        }
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

    fn execute_prepare(
        &self,
        name: &str,
        definition: &Statement,
        body: &UnifiedPlan,
    ) -> Result<SQLResult, SQLError> {
        if self.engine.lookup_prepared(name).is_some() {
            return Err(SQLError::Unsupported(format!(
                "Prepared statement `{name}` already exists"
            )));
        }
        self.engine
            .register_prepared_plan(name.to_string(), definition.clone(), body.clone());
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
        let mut scope = select::CteScope::new();
        for expression in params {
            scope.install_expression_plan(expression);
        }
        let hook = select::ScopedEngineHook::new(self.engine, &scope);
        let context = EvalContext::new(None, self.params).with_engine(&hook);
        let bound: Vec<SQLParam> = params
            .iter()
            .map(|expression| eval(&expression.expression, &context).map(SQLParam::Scalar))
            .collect::<Result<_, _>>()?;
        let driver = EngineUnifiedPlanDriver::new(self.engine, &bound);
        UnifiedPlanExecutor::new(&driver).execute(&plan)
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

    fn execute_merge(
        &self,
        statement: &MergeStmt,
        source: &SourcePlan,
        expressions: &[ExpressionPlan],
    ) -> Result<SQLResult, SQLError> {
        run_merge(
            self.engine,
            statement.clone(),
            source.clone(),
            expressions.to_vec(),
            self.params,
        )
    }

    fn execute_call(
        &self,
        name: &str,
        arguments: &[ExpressionPlan],
    ) -> Result<SQLResult, SQLError> {
        let mut scope = select::CteScope::new();
        for expression in arguments {
            scope.install_expression_plan(expression);
        }
        let hook = select::ScopedEngineHook::new(self.engine, &scope);
        let args: Vec<_> = arguments
            .iter()
            .map(|expression| expression.expression.clone())
            .collect();
        plpgsql_exec::run_call(self.engine, name, &args, self.params, &hook)
    }

    fn execute_command(&self, command: &CommandPlan) -> Result<SQLResult, SQLError> {
        match command {
            CommandPlan::CreateTable(statement) => run_create_table(self.engine, statement.clone()),
            CommandPlan::CreateIndex(statement) => run_create_index(self.engine, statement.clone()),
            CommandPlan::Insert {
                statement,
                source,
                expressions,
            } => self.execute_insert(statement, source.as_deref(), expressions),
            CommandPlan::Update {
                statement,
                ctes,
                source,
                expressions,
            } => self.execute_update(statement, ctes, source.as_deref(), expressions),
            CommandPlan::Delete {
                statement,
                ctes,
                source,
                expressions,
            } => self.execute_delete(statement, ctes, source.as_deref(), expressions),
            CommandPlan::Drop(statement) => run_drop(self.engine, statement.clone()),
            CommandPlan::AlterTable(statement) => {
                run_alter_table(self.engine, (**statement).clone())
            }
            CommandPlan::CreateView {
                name,
                definition,
                query,
                or_replace,
            } => self.execute_create_view(name, definition, query, *or_replace),
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
            CommandPlan::Prepare {
                name,
                definition,
                body,
            } => self.execute_prepare(name, definition, body),
            CommandPlan::Execute { name, params } => self.execute_prepared(name, params),
            CommandPlan::Deallocate { name } => self.execute_deallocate(name.as_deref()),
            CommandPlan::CreateForeignServer(statement) => {
                self.execute_create_foreign_server(statement)
            }
            CommandPlan::CreateForeignTable(statement) => {
                self.execute_create_foreign_table(statement)
            }
            CommandPlan::Merge {
                statement,
                source,
                expressions,
            } => self.execute_merge(statement, source, expressions),
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

impl UnifiedPlanDriver for EngineUnifiedPlanDriver<'_, '_> {
    type Error = SQLError;

    fn execute_plan(&self, plan: &UnifiedPlan) -> Result<SQLResult, Self::Error> {
        match plan {
            UnifiedPlan::Query(query) => self.execute_query(query),
            UnifiedPlan::Command(command) => self.execute_command(command),
        }
    }
}
