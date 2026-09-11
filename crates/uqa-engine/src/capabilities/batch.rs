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
use uqa_sql::{plan::UnifiedPlan, Statement};

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
    fn cached_sql_statement(&self, sql: &str) -> Option<CachedStatement> {
        Engine::cached_sql_statement(self, sql).map(|cached| CachedStatement {
            statement: cached.statement,
            logical_plan: cached.logical_plan,
            optimized_plan: cached.optimized_plan,
        })
    }
    fn cached_optimized_sql_plan(&self, sql: &str) -> Option<Arc<UnifiedPlan>> {
        Engine::cached_optimized_sql_plan(self, sql)
    }
    fn cache_sql_statement(
        &self,
        sql: String,
        statement: Arc<Statement>,
        logical_plan: Arc<UnifiedPlan>,
    ) {
        Engine::cache_sql_statement(self, sql, statement, logical_plan);
    }
    fn cache_optimized_sql_plan(&self, sql: &str, optimized_plan: Arc<UnifiedPlan>) {
        Engine::cache_optimized_sql_plan(self, sql, optimized_plan);
    }
}
