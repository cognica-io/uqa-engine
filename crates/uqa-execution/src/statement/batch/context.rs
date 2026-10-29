//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred execution inputs and session-owned cache, transaction and lock state.

use crate::{
    row_locks::retry_cache::RowLockRetryCache,
    statement::{
        context::{StatementEffects, StatementExecutionInputs, StatementRuntime},
        transactions::StatementTransactions,
    },
};
use std::sync::Arc;
use uqa_sql::{
    ast::TransactionStmt,
    plan::{AggregateClassifier, ExecutablePlanOptimizer, UnifiedPlan},
    SQLError, SQLParam, Statement,
};

/// Cancel the statement that begins now once the session's `statement_timeout` passes, as `PostgreSQL`'s `start_xact_command` enables the timeout for each statement of a query and `finish_xact_command` disables it; the returned deadline disarms when the statement ends.
pub fn statement_deadline<S: Clone + 'static>(
    context: &BatchExecutionContext<'_, S>,
) -> Option<uqa_core::CancellationDeadline> {
    let timeout = context.statements.statement_timeout()?;
    Some(
        context
            .runtime
            .cancellation
            .deadline(timeout, uqa_core::CancellationReason::StatementTimeout),
    )
}

/// Owned cache contents after the session has validated its catalog epochs.
pub struct CachedStatement {
    pub statement: Arc<Statement>,
    pub logical_plan: Arc<UnifiedPlan>,
    pub optimized_plan: Option<uqa_sql::plan::SharedExecutablePlan>,
    pub parser: uqa_sql::parser::ParserMetadata,
}

impl CachedStatement {
    /// Optimized entries were analyzed without parameters; any supplied inputs require native binding, including inputs the query does not reference.
    pub(crate) fn optimized_plan_for(
        &self,
        params: &[SQLParam],
    ) -> Option<&uqa_sql::plan::SharedExecutablePlan> {
        if params.is_empty() {
            self.optimized_plan.as_ref()
        } else {
            None
        }
    }
}

pub trait StatementCache {
    /// Look up analysis after the statement's snapshot and writer refreshes, with current catalog and scope validation. Ordinary cache implementations may decline this capability.
    fn cached_sql_analysis(
        &self,
        _sql: &str,
    ) -> Option<uqa_sql::binding::statements::AnalyzedStatement> {
        None
    }
    fn cache_sql_analysis(
        &self,
        _sql: &str,
        _analysis: Option<uqa_sql::binding::statements::AnalyzedStatement>,
    ) {
    }

    fn cached_sql_statement(&self, sql: &str) -> Option<CachedStatement>;
    fn cache_sql_statement(
        &self,
        sql: String,
        statement: Arc<Statement>,
        logical_plan: Arc<UnifiedPlan>,
        parser: uqa_sql::parser::ParserMetadata,
    );
    fn cache_optimized_sql_plan(
        &self,
        sql: &str,
        optimized_plan: uqa_sql::plan::SharedExecutablePlan,
    );
}

/// Transaction state changes requested at the native scheduler's boundaries.
pub trait BatchTransactions: StatementTransactions {
    /// Refresh an idle session before reading its statement cache. An active transaction retains its view until its admitted statement selects a snapshot, including ownership of refresh failure.
    fn refresh_idle_statement_view(&self) -> Result<(), SQLError>;
    fn begin_simple_query_transaction(&self) -> Result<(), SQLError>;
    fn promote_simple_query_transaction(&self) -> Result<(), SQLError>;
    fn run_transaction_statement(&self, statement: TransactionStmt) -> Result<(), SQLError>;
    /// Admit and run one operation in the existing transaction. The owner rejects failed or unresolved frames before invoking the callback, invokes it exactly once on admission, and owns error and unwind cleanup.
    fn with_active_transaction(
        &self,
        operation: &mut dyn FnMut() -> Result<(), SQLError>,
    ) -> Result<(), SQLError>;
    fn prepare_explicit_statement_snapshot(&self, sets_snapshot: bool) -> Result<(), SQLError>;
    fn prepare_explicit_transaction_writer(&self) -> Result<bool, SQLError>;
    fn begin_implicit_statement_transaction(&self, read_only: bool) -> Result<(), SQLError>;
}

pub(in crate::statement) fn with_active_transaction<T>(
    transactions: &dyn BatchTransactions,
    operation: impl FnOnce() -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let mut operation = Some(operation);
    let mut result = None;
    transactions.with_active_transaction(&mut || {
        let operation = operation.take().ok_or_else(|| {
            SQLError::Internal("transaction owner invoked its operation more than once".into())
        })?;
        result = Some(operation()?);
        Ok(())
    })?;
    result.ok_or_else(|| {
        SQLError::Internal("transaction owner did not invoke its admitted operation".into())
    })
}

/// Retain the session's row-lock frame until the current loop iteration exits.
pub trait RowLockStatementGuard {}

pub trait BatchRowLocks {
    fn begin_row_lock_statement(&self) -> Box<dyn RowLockStatementGuard + '_>;
    fn statement_row_lock_cache(&self) -> Result<Arc<RowLockRetryCache>, SQLError>;
}

pub struct BatchExecutionContext<'a, S: Clone + 'static> {
    pub runtime: StatementRuntime<'a>,
    pub persistent_backend: bool,
    pub statements: &'a dyn StatementExecutionInputs<S>,
    pub cache: &'a dyn StatementCache,
    pub aggregates: &'a dyn AggregateClassifier,
    pub effects: &'a dyn StatementEffects,
    pub planning: &'a dyn ExecutablePlanOptimizer,
    pub transactions: &'a dyn BatchTransactions,
    pub row_locks: &'a dyn BatchRowLocks,
}
