//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind native batch scheduling to the existing session state and cache.

use crate::{session::StatementReadSnapshot, Engine};
use std::sync::Arc;
use uqa_execution::statement::{
    batch::context::{BatchExecutionContext, CachedStatement, StatementCache},
    context::StatementRuntime,
};
use uqa_sql::{SQLError, Statement};

impl Engine {
    pub(crate) fn batch_execution_context(
        &self,
    ) -> BatchExecutionContext<'_, StatementReadSnapshot> {
        BatchExecutionContext {
            runtime: StatementRuntime {
                cancellation: &self.runtime.cancellation,
                notices: &self.runtime.notices,
            },
            persistent_backend: self.storage.backend.is_some(),
            statements: self,
            cache: self,
            aggregates: self,
            effects: self,
            planning: self,
            transactions: self,
            row_locks: self,
        }
    }
}

impl StatementCache for Engine {
    fn cached_sql_analysis(
        &self,
        sql: &str,
    ) -> Option<uqa_sql::binding::statements::AnalyzedStatement> {
        Engine::cached_sql_analysis(self, sql)
    }
    fn cache_sql_analysis(
        &self,
        sql: &str,
        analysis: Option<uqa_sql::binding::statements::AnalyzedStatement>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        Engine::cache_sql_analysis(self, sql, analysis, control)
    }

    fn cached_sql_statement(&self, sql: &str) -> Option<CachedStatement> {
        Engine::cached_sql_statement(self, sql).map(|cached| CachedStatement {
            statement: cached.statement,
            logical_plan: cached.logical_plan,
            optimized_plan: cached.optimized_plan,
            parser: cached.parser,
        })
    }
    fn cache_sql_statement(
        &self,
        sql: &str,
        statement: Arc<Statement>,
        logical_plan: uqa_sql::plan::SharedExecutablePlan,
        parser: uqa_sql::parser::ParserMetadata,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        Engine::cache_sql_statement(self, sql, statement, logical_plan, parser, control)
    }
    fn cache_optimized_sql_plan(
        &self,
        sql: &str,
        optimized_plan: uqa_sql::plan::SharedExecutablePlan,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        Engine::cache_optimized_sql_plan(self, sql, optimized_plan, control)
    }
}
